use crate::{
    error::ExportError,
    export::{Export, Exportable},
    import::LikeNamespace,
    module::{ExportIndex, Module},
    new,
    structures::TypedIndex,
    typed_func::Func,
    types::Value,
    vm,
};
use std::{
    cell::{Ref, RefCell, RefMut},
    error::Error,
    rc::Rc,
};

pub use crate::typed_func::DynamicFunc as DynFunc;

#[derive(Debug)]
pub(crate) struct PreInstance {
    pub(crate) vmctx: Rc<RefCell<vm::Ctx>>,
}

impl PreInstance {
    pub(crate) fn new() -> Self {
        Self {
            vmctx: Rc::new(RefCell::new(unsafe { vm::Ctx::new_uninit() })),
        }
    }

    pub(crate) fn vmctx(&self) -> Rc<RefCell<vm::Ctx>> {
        self.vmctx.clone()
    }

    pub(crate) fn vmctx_ptr(&self) -> *mut vm::Ctx {
        self.vmctx.as_ptr()
    }
}

// #[derive(Clone)]
pub struct Instance {
    pre_instance: Box<PreInstance>,
    pub exports: Exports,
    pub(crate) new_instance: new::wasmer::Instance,
}

impl Instance {
    pub(crate) fn new(pre_instance: Box<PreInstance>, new_instance: new::wasmer::Instance) -> Self {
        // Initialize the `vm::Ctx`
        {
            let mut vmctx = pre_instance.vmctx.borrow_mut();

            vmctx.module_info = new_instance.module().info() as *const _;
        }

        Self {
            pre_instance,
            exports: new_instance.exports.clone().into(),
            new_instance,
        }
    }

    pub fn load<T>(&self, _loader: T) -> Result<Self, ()> {
        Err(())
    }

    pub fn func<Args, Rets>(&self, name: &str) -> Result<Func<Args, Rets>, ExportError>
    where
        Args: new::wasmer::WasmTypeList + Clone,
        Rets: new::wasmer::WasmTypeList + Clone,
    {
        self.exports.get(name)
    }

    pub fn dyn_func(&self, name: &str) -> Result<DynFunc, ExportError> {
        self.exports.get(name)
    }

    pub fn resolve_func(&self, name: &str) -> Result<usize, ()> {
        self.new_instance
            .module()
            .info()
            .exports
            .iter()
            .find_map(|(export_name, export_index)| {
                if name == export_name {
                    match export_index {
                        ExportIndex::Function(index) => Some(index.index()),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .ok_or(())
    }

    pub fn call(&self, name: &str, params: &[Value]) -> Result<Vec<Value>, Box<dyn Error>> {
        Ok(self
            .new_instance
            .exports
            .get_function(name)?
            .call(params)?
            .into_vec())
    }

    pub fn module(&self) -> Module {
        Module::new(self.new_instance.module().clone())
    }

    pub fn context(&self) -> Ref<vm::Ctx> {
        self.pre_instance.vmctx.borrow()
    }

    pub fn context_mut(&mut self) -> RefMut<vm::Ctx> {
        self.pre_instance.vmctx.borrow_mut()
    }

    pub fn exports(
        &self,
    ) -> new::wasmer::ExportsIterator<impl Iterator<Item = (&String, &Export)>> {
        self.new_instance.exports.iter()
    }
}

impl LikeNamespace for Instance {
    fn get_namespace_export(&self, name: &str) -> Option<new::wasmer_runtime::Export> {
        self.exports.new_exports.get_namespace_export(name)
    }

    fn get_namespace_exports(&self) -> Vec<(String, new::wasmer_runtime::Export)> {
        self.exports.new_exports.get_namespace_exports()
    }
}

#[derive(Clone)]
pub struct Exports {
    pub(crate) new_exports: new::wasmer::Exports,
}

impl Exports {
    pub fn new() -> Self {
        Self {
            new_exports: new::wasmer::Exports::new(),
        }
    }

    pub fn get<'a, T>(&'a self, name: &str) -> Result<T, ExportError>
    where
        T: Exportable<'a> + Clone + 'a,
    {
        Ok(self.new_exports.get::<T>(name)?.clone())
    }
}

impl LikeNamespace for Exports {
    fn get_namespace_export(&self, name: &str) -> Option<new::wasmer_runtime::Export> {
        self.new_exports.get_namespace_export(name)
    }

    fn get_namespace_exports(&self) -> Vec<(String, new::wasmer_runtime::Export)> {
        self.new_exports.get_namespace_exports()
    }
}

impl From<new::wasmer::Exports> for Exports {
    fn from(new_exports: new::wasmer::Exports) -> Self {
        Self { new_exports }
    }
}
