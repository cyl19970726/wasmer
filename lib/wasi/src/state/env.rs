use std::{
    collections::HashMap,
    ops::Deref,
    path::PathBuf,
    sync::{Arc, RwLockReadGuard, RwLockWriteGuard},
};

use crate::{
    bin_factory::ModuleCache, fs::WasiFsRoot, import_object_for_all_wasi_versions,
    runtime::SpawnType, SpawnedMemory, WasiControlPlane, WasiFunctionEnv, WasiRuntimeError,
};
use derivative::Derivative;
use tracing::{trace, warn};
use wasmer::{
    AsStoreMut, AsStoreRef, FunctionEnvMut, Global, Instance, Memory, MemoryView, Module,
    TypedFunction,
};
use wasmer_vnet::DynVirtualNetworking;
use wasmer_wasi_types::{
    types::Signal,
    wasi::{Errno, ExitCode, Snapshot0Clockid},
};

use crate::{
    bin_factory::BinFactory,
    fs::WasiInodes,
    os::{
        command::builtins::cmd_wasmer::CmdWasmer,
        task::{
            control_plane::ControlPlaneError,
            process::{WasiProcess, WasiProcessId},
            thread::{WasiThread, WasiThreadHandle, WasiThreadId},
        },
    },
    syscalls::platform_clock_time_get,
    VirtualTaskManager, WasiError, WasiRuntime, WasiState, WasiStateCreationError,
    WasiVFork, DEFAULT_STACK_SIZE,
};

use super::Capabilities;

/// Various [`TypedFunction`] and [`Global`] handles for an active WASI(X) instance.
///
/// Used to access and modify runtime state.
// TODO: make fields private
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct WasiInstanceHandles {
    // TODO: the two fields below are instance specific, while all others are module specific.
    // Should be split up.
    /// Represents a reference to the memory
    pub(crate) memory: Memory,
    pub(crate) instance: wasmer::Instance,

    /// Points to the current location of the memory stack pointer
    pub(crate) stack_pointer: Option<Global>,

    /// Main function that will be invoked (name = "_start")
    #[derivative(Debug = "ignore")]
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) start: Option<TypedFunction<(), ()>>,

    /// Function thats invoked to initialize the WASM module (nane = "_initialize")
    #[derivative(Debug = "ignore")]
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) initialize: Option<TypedFunction<(), ()>>,

    /// Represents the callback for spawning a thread (name = "_start_thread")
    /// (due to limitations with i64 in browsers the parameters are broken into i32 pairs)
    /// [this takes a user_data field]
    #[derivative(Debug = "ignore")]
    pub(crate) thread_spawn: Option<TypedFunction<(i32, i32), ()>>,

    /// Represents the callback for spawning a reactor (name = "_react")
    /// (due to limitations with i64 in browsers the parameters are broken into i32 pairs)
    /// [this takes a user_data field]
    #[derivative(Debug = "ignore")]
    pub(crate) react: Option<TypedFunction<(i32, i32), ()>>,

    /// Represents the callback for signals (name = "__wasm_signal")
    /// Signals are triggered asynchronously at idle times of the process
    #[derivative(Debug = "ignore")]
    pub(crate) signal: Option<TypedFunction<i32, ()>>,

    /// Flag that indicates if the signal callback has been set by the WASM
    /// process - if it has not been set then the runtime behaves differently
    /// when a CTRL-C is pressed.
    pub(crate) signal_set: bool,

    /// Represents the callback for destroying a local thread variable (name = "_thread_local_destroy")
    /// [this takes a pointer to the destructor and the data to be destroyed]
    #[derivative(Debug = "ignore")]
    pub(crate) thread_local_destroy: Option<TypedFunction<(i32, i32, i32, i32), ()>>,

    /// asyncify_start_unwind(data : i32): call this to start unwinding the
    /// stack from the current location. "data" must point to a data
    /// structure as described above (with fields containing valid data).
    #[derivative(Debug = "ignore")]
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) asyncify_start_unwind: Option<TypedFunction<i32, ()>>,

    /// asyncify_stop_unwind(): call this to note that unwinding has
    /// concluded. If no other code will run before you start to rewind,
    /// this is not strictly necessary, however, if you swap between
    /// coroutines, or even just want to run some normal code during a
    /// "sleep", then you must call this at the proper time. Otherwise,
    /// the code will think it is still unwinding when it should not be,
    /// which means it will keep unwinding in a meaningless way.
    #[derivative(Debug = "ignore")]
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) asyncify_stop_unwind: Option<TypedFunction<(), ()>>,

    /// asyncify_start_rewind(data : i32): call this to start rewinding the
    /// stack vack up to the location stored in the provided data. This prepares
    /// for the rewind; to start it, you must call the first function in the
    /// call stack to be unwound.
    #[derivative(Debug = "ignore")]
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) asyncify_start_rewind: Option<TypedFunction<i32, ()>>,

    /// asyncify_stop_rewind(): call this to note that rewinding has
    /// concluded, and normal execution can resume.
    #[derivative(Debug = "ignore")]
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) asyncify_stop_rewind: Option<TypedFunction<(), ()>>,

    /// asyncify_get_state(): call this to get the current value of the
    /// internal "__asyncify_state" variable as described above.
    /// It can be used to distinguish between unwinding/rewinding and normal
    /// calls, so that you know when to start an asynchronous operation and
    /// when to propagate results back.
    #[allow(dead_code)]
    #[derivative(Debug = "ignore")]
    pub(crate) asyncify_get_state: Option<TypedFunction<(), i32>>,
}

impl WasiInstanceHandles {
    pub fn new(memory: Memory, store: &impl AsStoreRef, instance: Instance) -> Self {
        WasiInstanceHandles {
            memory,
            stack_pointer: instance
                .exports
                .get_global("__stack_pointer")
                .map(|a| a.clone())
                .ok(),
            start: instance.exports.get_typed_function(store, "_start").ok(),
            initialize: instance
                .exports
                .get_typed_function(store, "_initialize")
                .ok(),
            thread_spawn: instance
                .exports
                .get_typed_function(store, "_start_thread")
                .ok(),
            react: instance.exports.get_typed_function(store, "_react").ok(),
            signal: instance
                .exports
                .get_typed_function(store, "__wasm_signal")
                .ok(),
            signal_set: false,
            asyncify_start_unwind: instance
                .exports
                .get_typed_function(store, "asyncify_start_unwind")
                .ok(),
            asyncify_stop_unwind: instance
                .exports
                .get_typed_function(store, "asyncify_stop_unwind")
                .ok(),
            asyncify_start_rewind: instance
                .exports
                .get_typed_function(store, "asyncify_start_rewind")
                .ok(),
            asyncify_stop_rewind: instance
                .exports
                .get_typed_function(store, "asyncify_stop_rewind")
                .ok(),
            asyncify_get_state: instance
                .exports
                .get_typed_function(store, "asyncify_get_state")
                .ok(),
            thread_local_destroy: instance
                .exports
                .get_typed_function(store, "_thread_local_destroy")
                .ok(),
            instance,
        }
    }
}

/// The code itself makes safe use of the struct so multiple threads don't access
/// it (without this the JS code prevents the reference to the module from being stored
/// which is needed for the multithreading mode)
unsafe impl Send for WasiInstanceHandles {}

unsafe impl Sync for WasiInstanceHandles {}

/// Data required to construct a [`WasiEnv`].
#[derive(Debug)]
pub(crate) struct WasiEnvInit {
    pub state: WasiState,
    pub runtime: Arc<dyn WasiRuntime + Send + Sync>,
    pub module_cache: Arc<ModuleCache>,
    pub webc_dependencies: Vec<String>,
    pub mapped_commands: HashMap<String, PathBuf>,
    pub control_plane: WasiControlPlane,
    pub bin_factory: BinFactory,
    pub capabilities: Capabilities,

    // TODO: remove these again?
    pub spawn_type: Option<SpawnType>,
    pub process: WasiProcess,
    pub thread: Option<WasiThread>,

    /// Whether to call the `_initialize` function in the WASI module.
    /// Will be true for regular new instances, but false for threads.
    pub call_initialize: bool,
}

/// The environment provided to the WASI imports.
#[derive(Debug, Clone)]
pub struct WasiEnv {
    /// Represents the process this environment is attached to
    pub process: WasiProcess,
    /// Represents the thread this environment is attached to
    pub thread: WasiThread,
    /// Represents a fork of the process that is currently in play
    pub vfork: Option<WasiVFork>,
    /// Base stack pointer for the memory stack
    pub stack_base: u64,
    /// Start of the stack memory that is allocated for this thread
    pub stack_start: u64,
    /// Shared state of the WASI system. Manages all the data that the
    /// executing WASI program can see.
    pub state: Arc<WasiState>,
    /// Binary factory attached to this environment
    pub bin_factory: BinFactory,
    /// Inner functions and references that are loaded before the environment starts
    pub inner: Option<WasiInstanceHandles>,
    /// List of the handles that are owned by this context
    /// (this can be used to ensure that threads own themselves or others)
    pub owned_handles: Vec<WasiThreadHandle>,
    /// Implementation of the WASI runtime.
    pub runtime: Arc<dyn WasiRuntime + Send + Sync + 'static>,
    pub module_cache: Arc<ModuleCache>,

    pub capabilities: Capabilities,
}

// FIXME: remove unsafe impls!
// Added because currently WasiEnv can hold a wasm_bindgen::JsValue via wasmer::Module.
#[cfg(feature = "js")]
unsafe impl Send for WasiEnv {}
#[cfg(feature = "js")]
unsafe impl Sync for WasiEnv {}

impl WasiEnv {
    /// Forking the WasiState is used when either fork or vfork is called
    pub fn fork(&self) -> Result<(Self, WasiThreadHandle), ControlPlaneError> {
        let process = self.process.compute.new_process()?;
        let handle = process.new_thread()?;

        let thread = handle.as_thread();
        thread.copy_stack_from(&self.thread);

        let state = Arc::new(self.state.fork(true));

        let bin_factory = self.bin_factory.clone();

        let new_env = Self {
            process,
            thread,
            vfork: None,
            stack_base: self.stack_base,
            stack_start: self.stack_start,
            bin_factory,
            state,
            inner: None,
            owned_handles: Vec::new(),
            runtime: self.runtime.clone(),
            capabilities: self.capabilities.clone(),
            module_cache: self.module_cache.clone(),
        };
        Ok((new_env, handle))
    }

    pub fn pid(&self) -> WasiProcessId {
        self.process.pid()
    }

    pub fn tid(&self) -> WasiThreadId {
        self.thread.tid()
    }
}

impl WasiEnv {
    pub fn builder(program_name: impl Into<String>) -> WasiEnvBuilder {
        WasiEnvBuilder::new(program_name)
    }

    #[deprecated(note = "Use `WasiEnv::instantiate` instead")]
    pub(crate) fn from_init(init: WasiEnvInit) -> Result<Self, anyhow::Error> {
        let process = init.process;
        let thread = process.new_thread()?;

        let mut env = Self {
            process,
            thread: thread.as_thread(),
            vfork: None,
            stack_base: DEFAULT_STACK_SIZE,
            stack_start: 0,
            state: Arc::new(init.state),
            inner: None,
            owned_handles: Vec::new(),
            runtime: init.runtime,
            bin_factory: init.bin_factory,
            module_cache: init.module_cache.clone(),
            capabilities: init.capabilities,
        };
        env.owned_handles.push(thread);

        Ok(env)
    }

    // FIXME: use custom error type
    pub(crate) fn instantiate(
        init: WasiEnvInit,
        module: Module,
        store: &mut impl AsStoreMut,
    ) -> Result<(Instance, WasiFunctionEnv), WasiRuntimeError> {
        let process = init.process;
        let thread = process.new_thread()?;

        let mut env = Self {
            process,
            thread: thread.as_thread(),
            vfork: None,
            stack_base: DEFAULT_STACK_SIZE,
            stack_start: 0,
            state: Arc::new(init.state),
            inner: None,
            owned_handles: Vec::new(),
            runtime: init.runtime,
            bin_factory: init.bin_factory,
            module_cache: init.module_cache.clone(),
            capabilities: init.capabilities,
        };
        env.owned_handles.push(thread);

        // TODO: should not be here - should be callers responsibility!
        env.uses(init.webc_dependencies)?;

        #[cfg(feature = "sys")]
        env.map_commands(init.mapped_commands.clone())?;

        let pid = env.process.pid();

        let mut store = store.as_store_mut();

        let tasks = env.runtime.task_manager().clone();
        let mut func_env = WasiFunctionEnv::new(&mut store, env);

        // Determine if shared memory needs to be created and imported
        let shared_memory = module.imports().memories().next().map(|a| *a.ty());

        // Determine if we are going to create memory and import it or just rely on self creation of memory
        let spawn_type = if let Some(t) = init.spawn_type {
            t
        } else {
            match shared_memory {
                Some(ty) => {
                    #[cfg(feature = "sys")]
                    let style = store.tunables().memory_style(&ty);
                    SpawnType::CreateWithType(SpawnedMemory {
                        ty,
                        #[cfg(feature = "sys")]
                        style,
                    })
                }
                None => SpawnType::Create,
            }
        };
        let memory = tasks.build_memory(spawn_type)?;

        // Let's instantiate the module with the imports.
        let (mut import_object, instance_init_callback) =
            import_object_for_all_wasi_versions(&module, &mut store, &func_env.env);
        if let Some(memory) = memory {
            import_object.define(
                "env",
                "memory",
                Memory::new_from_existing(&mut store, memory),
            );
        }

        // Construct the instance.
        let instance = match Instance::new(&mut store, &module, &import_object) {
            Ok(a) => a,
            Err(err) => {
                tracing::error!("wasi[{}]::wasm instantiate error ({})", pid, err);
                func_env
                    .data(&store)
                    .cleanup(Some(Errno::Noexec as ExitCode));
                return Err(err.into());
            }
        };

        // Run initializers.
        instance_init_callback(&instance, &store).unwrap();

        // Initialize the WASI environment
        if let Err(err) = func_env.initialize(&mut store, instance.clone()) {
            tracing::error!("wasi[{}]::wasi initialize error ({})", pid, err);
            func_env
                .data(&store)
                .cleanup(Some(Errno::Noexec as ExitCode));
            return Err(err.into());
        }

        // If this module exports an _initialize function, run that first.
        if init.call_initialize {
            if let Ok(initialize) = instance.exports.get_function("_initialize") {
                if let Err(err) = crate::run_wasi_func_start(initialize, &mut store) {
                    func_env
                        .data(&store)
                        .cleanup(Some(Errno::Noexec as ExitCode));
                    return Err(err);
                }
            }
        }

        Ok((instance, func_env))
    }

    /// Returns a copy of the current runtime implementation for this environment
    pub fn runtime(&self) -> &(dyn WasiRuntime) {
        self.runtime.deref()
    }

    /// Returns a copy of the current tasks implementation for this environment
    pub fn tasks(&self) -> &Arc<dyn VirtualTaskManager> {
        self.runtime.task_manager()
    }

    pub fn fs_root(&self) -> &WasiFsRoot {
        &self.state.fs.root_fs
    }

    /// Overrides the runtime implementation for this environment
    pub fn set_runtime<R>(&mut self, runtime: R)
    where
        R: WasiRuntime + Send + Sync + 'static,
    {
        self.runtime = Arc::new(runtime);
    }

    /// Returns the number of active threads
    pub fn active_threads(&self) -> u32 {
        self.process.active_threads()
    }

    /// Porcesses any signals that are batched up or any forced exit codes
    pub fn process_signals_and_exit(
        ctx: &mut FunctionEnvMut<'_, Self>,
    ) -> Result<Result<bool, Errno>, WasiError> {
        // If a signal handler has never been set then we need to handle signals
        // differently
        let env = ctx.data();
        if !env.inner().signal_set {
            let signals = env.thread.pop_signals();
            let signal_cnt = signals.len();
            for sig in signals {
                if sig == Signal::Sigint || sig == Signal::Sigquit || sig == Signal::Sigkill {
                    env.thread.terminate(Errno::Intr as u32);
                    return Err(WasiError::Exit(Errno::Intr as u32));
                } else {
                    trace!("wasi[{}]::signal-ignored: {:?}", env.pid(), sig);
                }
            }
            return Ok(Ok(signal_cnt > 0));
        }

        // Check for forced exit
        if let Some(forced_exit) = env.should_exit() {
            return Err(WasiError::Exit(forced_exit));
        }

        Ok(Self::process_signals(ctx)?)
    }

    /// Porcesses any signals that are batched up
    pub fn process_signals(
        ctx: &mut FunctionEnvMut<'_, Self>,
    ) -> Result<Result<bool, Errno>, WasiError> {
        // If a signal handler has never been set then we need to handle signals
        // differently
        let env = ctx.data();
        if !env.inner().signal_set {
            if env
                .thread
                .has_signal(&[Signal::Sigint, Signal::Sigquit, Signal::Sigkill])
            {
                env.thread.terminate(Errno::Intr as u32);
            }
            return Ok(Ok(false));
        }

        // Check for any signals that we need to trigger
        // (but only if a signal handler is registered)
        if let Some(_) = env.inner().signal.as_ref() {
            let signals = env.thread.pop_signals();
            Ok(Ok(Self::process_signals_internal(ctx, signals)?))
        } else {
            Ok(Ok(false))
        }
    }

    pub fn process_signals_internal(
        ctx: &mut FunctionEnvMut<'_, Self>,
        mut signals: Vec<Signal>,
    ) -> Result<bool, WasiError> {
        let env = ctx.data();
        if let Some(handler) = env.inner().signal.clone() {
            // We might also have signals that trigger on timers
            let mut now = 0;
            let has_signal_interval = {
                let mut any = false;
                let inner = env.process.inner.read().unwrap();
                if !inner.signal_intervals.is_empty() {
                    now = platform_clock_time_get(Snapshot0Clockid::Monotonic, 1_000_000).unwrap()
                        as u128;
                    for signal in inner.signal_intervals.values() {
                        let elapsed = now - signal.last_signal;
                        if elapsed >= signal.interval.as_nanos() {
                            any = true;
                            break;
                        }
                    }
                }
                any
            };
            if has_signal_interval {
                let mut inner = env.process.inner.write().unwrap();
                for signal in inner.signal_intervals.values_mut() {
                    let elapsed = now - signal.last_signal;
                    if elapsed >= signal.interval.as_nanos() {
                        signal.last_signal = now;
                        signals.push(signal.signal);
                    }
                }
            }

            for signal in signals {
                tracing::trace!(
                    "wasi[{}]::processing-signal: {:?}",
                    ctx.data().pid(),
                    signal
                );
                if let Err(err) = handler.call(ctx, signal as i32) {
                    match err.downcast::<WasiError>() {
                        Ok(wasi_err) => {
                            warn!(
                                "wasi[{}]::signal handler wasi error - {}",
                                ctx.data().pid(),
                                wasi_err
                            );
                            return Err(wasi_err);
                        }
                        Err(runtime_err) => {
                            warn!(
                                "wasi[{}]::signal handler runtime error - {}",
                                ctx.data().pid(),
                                runtime_err
                            );
                            return Err(WasiError::Exit(Errno::Intr as ExitCode));
                        }
                    }
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Returns an exit code if the thread or process has been forced to exit
    pub fn should_exit(&self) -> Option<u32> {
        // Check for forced exit
        if let Some(forced_exit) = self.thread.try_join() {
            return Some(forced_exit);
        }
        if let Some(forced_exit) = self.process.try_join() {
            return Some(forced_exit);
        }
        None
    }

    /// Accesses the virtual networking implementation
    pub fn net(&self) -> DynVirtualNetworking {
        self.runtime.networking()
    }

    /// Providers safe access to the initialized part of WasiEnv
    /// (it must be initialized before it can be used)
    pub fn inner(&self) -> &WasiInstanceHandles {
        self.inner
            .as_ref()
            .expect("You must initialize the WasiEnv before using it")
    }

    /// Providers safe access to the initialized part of WasiEnv
    /// (it must be initialized before it can be used)
    pub fn inner_mut(&mut self) -> &mut WasiInstanceHandles {
        self.inner
            .as_mut()
            .expect("You must initialize the WasiEnv before using it")
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    pub fn memory_view<'a>(&'a self, store: &'a impl AsStoreRef) -> MemoryView<'a> {
        self.memory().view(store)
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    pub fn memory(&self) -> &Memory {
        &self.inner().memory
    }

    /// Copy the lazy reference so that when it's initialized during the
    /// export phase, all the other references get a copy of it
    pub fn memory_clone(&self) -> Memory {
        self.memory().clone()
    }

    /// Get the WASI state
    pub fn state(&self) -> &WasiState {
        &self.state
    }

    pub(crate) fn get_memory_and_wasi_state<'a>(
        &'a self,
        store: &'a impl AsStoreRef,
        _mem_index: u32,
    ) -> (MemoryView<'a>, &WasiState) {
        let memory = self.memory_view(store);
        let state = self.state.deref();
        (memory, state)
    }

    pub(crate) fn get_memory_and_wasi_state_and_inodes<'a>(
        &'a self,
        store: &'a impl AsStoreRef,
        _mem_index: u32,
    ) -> (MemoryView<'a>, &WasiState, RwLockReadGuard<WasiInodes>) {
        let memory = self.memory_view(store);
        let state = self.state.deref();
        let inodes = state.inodes.read().unwrap();
        (memory, state, inodes)
    }

    pub(crate) fn get_memory_and_wasi_state_and_inodes_mut<'a>(
        &'a self,
        store: &'a impl AsStoreRef,
        _mem_index: u32,
    ) -> (MemoryView<'a>, &WasiState, RwLockWriteGuard<WasiInodes>) {
        let memory = self.memory_view(store);
        let state = self.state.deref();
        let inodes = state.inodes.write().unwrap();
        (memory, state, inodes)
    }

    pub fn uses<I>(&self, uses: I) -> Result<(), WasiStateCreationError>
    where
        I: IntoIterator<Item = String>,
    {
        // Load all the containers that we inherit from
        #[allow(unused_imports)]
        use std::path::Path;
        use std::{borrow::Cow, collections::VecDeque};

        #[allow(unused_imports)]
        use wasmer_vfs::FileSystem;

        let mut already: HashMap<String, Cow<'static, str>> = HashMap::new();

        let mut use_packages = uses.into_iter().collect::<VecDeque<_>>();

        let cmd_wasmer = self
            .bin_factory
            .commands
            .get("/bin/wasmer")
            .and_then(|cmd| cmd.as_any().downcast_ref::<CmdWasmer>());

        while let Some(use_package) = use_packages.pop_back() {
            if let Some(package) = cmd_wasmer
                .as_ref()
                .and_then(|cmd| cmd.get_package(use_package.clone(), self.tasks().deref()))
            {
                // If its already been added make sure the version is correct
                let package_name = package.package_name.to_string();
                if let Some(version) = already.get(&package_name) {
                    if version.as_ref() != package.version.as_ref() {
                        return Err(WasiStateCreationError::WasiInheritError(format!(
                            "webc package version conflict for {} - {} vs {}",
                            use_package, version, package.version
                        )));
                    }
                    continue;
                }
                already.insert(package_name, package.version.clone());

                // Add the additional dependencies
                for dependency in package.uses.clone() {
                    use_packages.push_back(dependency);
                }

                if let WasiFsRoot::Sandbox(root_fs) = &self.state.fs.root_fs {
                    // We first need to copy any files in the package over to the temporary file system
                    if let Some(fs) = package.webc_fs.as_ref() {
                        root_fs.union(fs);
                    }

                    // Add all the commands as binaries in the bin folder
                    let commands = package.commands.read().unwrap();
                    if !commands.is_empty() {
                        let _ = root_fs.create_dir(Path::new("/bin"));
                        for command in commands.iter() {
                            let path = format!("/bin/{}", command.name);
                            let path = Path::new(path.as_str());
                            if let Err(err) = root_fs
                                .new_open_options_ext()
                                .insert_ro_file(path, command.atom.clone())
                            {
                                tracing::debug!(
                                    "failed to add package [{}] command [{}] - {}",
                                    use_package,
                                    command.name,
                                    err
                                );
                                continue;
                            }

                            // Add the binary package to the bin factory (zero copy the atom)
                            let mut package = package.clone();
                            package.entry = Some(command.atom.clone());
                            self.bin_factory
                                .set_binary(path.as_os_str().to_string_lossy().as_ref(), package);
                        }
                    }
                } else {
                    return Err(WasiStateCreationError::WasiInheritError(
                        "failed to add package as the file system is not sandboxed".to_string(),
                    ));
                }
            } else {
                return Err(WasiStateCreationError::WasiInheritError(format!(
                    "failed to fetch webc package for {}",
                    use_package
                )));
            }
        }
        Ok(())
    }

    #[cfg(feature = "sys")]
    pub fn map_commands(
        &self,
        map_commands: std::collections::HashMap<String, std::path::PathBuf>,
    ) -> Result<(), WasiStateCreationError> {
        // Load all the mapped atoms
        #[allow(unused_imports)]
        use std::path::Path;

        #[allow(unused_imports)]
        use wasmer_vfs::FileSystem;

        #[cfg(feature = "sys")]
        for (command, target) in map_commands.iter() {
            // Read the file
            let file = std::fs::read(target).map_err(|err| {
                WasiStateCreationError::WasiInheritError(format!(
                    "failed to read local binary [{}] - {}",
                    target.as_os_str().to_string_lossy(),
                    err
                ))
            })?;
            let file: std::borrow::Cow<'static, [u8]> = file.into();

            if let WasiFsRoot::Sandbox(root_fs) = &self.state.fs.root_fs {
                let _ = root_fs.create_dir(Path::new("/bin"));

                let path = format!("/bin/{}", command);
                let path = Path::new(path.as_str());
                if let Err(err) = root_fs.new_open_options_ext().insert_ro_file(path, file) {
                    tracing::debug!("failed to add atom command [{}] - {}", command, err);
                    continue;
                }
            } else {
                tracing::debug!("failed to add atom command [{}] to the root file system as it is not sandboxed", command);
                continue;
            }
        }
        Ok(())
    }

    /// Cleans up all the open files (if this is the main thread)
    pub fn cleanup(&self, exit_code: Option<ExitCode>) {
        // If this is the main thread then also close all the files
        if self.thread.is_main() {
            trace!("wasi[{}]:: cleaning up open file handles", self.pid());

            let inodes = self.state.inodes.read().unwrap();
            self.state.fs.close_all(inodes.deref());

            // Now send a signal that the thread is terminated
            self.process.signal_process(Signal::Sigquit);

            // Terminate the process
            let exit_code = exit_code.unwrap_or(Errno::Canceled as ExitCode);
            self.process.terminate(exit_code);
        }
    }
}
