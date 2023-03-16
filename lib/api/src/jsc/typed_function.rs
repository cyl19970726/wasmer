//! Native Functions.
//!
//! This module creates the helper `TypedFunction` that let us call WebAssembly
//! functions with the native ABI, that is:
//!
//! ```ignore
//! let add_one = instance.exports.get_function("function_name")?;
//! let add_one_native: TypedFunction<i32, i32> = add_one.typed().unwrap();
//! ```
use crate::jsc::as_js::{param_from_js, AsJs};
use crate::native_type::NativeWasmTypeInto;
use crate::Value;
use crate::{AsStoreMut, TypedFunction};
use crate::{FromToNativeWasmType, RuntimeError, WasmTypeList};
use rusty_jsc::JSValue;
// use std::panic::{catch_unwind, AssertUnwindSafe};
use std::iter::FromIterator;
use wasmer_types::RawValue;

macro_rules! impl_native_traits {
    (  $( $x:ident ),* ) => {
        #[allow(unused_parens, non_snake_case)]
        impl<$( $x , )* Rets> TypedFunction<( $( $x ),* ), Rets>
        where
            $( $x: FromToNativeWasmType, )*
            Rets: WasmTypeList,
        {
            /// Call the typed func and return results.
            #[allow(clippy::too_many_arguments)]
            pub fn call(&self, mut store: &mut impl AsStoreMut, $( $x: $x, )* ) -> Result<Rets, RuntimeError> where
            $( $x: FromToNativeWasmType + NativeWasmTypeInto, )*
            {
                #[allow(unused_unsafe)]
                let params_list: Vec<_> = unsafe {
                    vec![ $( {
                        let raw = $x.into_raw(store);
                        let value = Value::from_raw(&mut store, $x::WASM_TYPE, dbg!(raw));
                        value.as_jsvalue(store)
                    } ),* ]
                };
                println!("TYPED FUNCTION CALL: {}", params_list.len());
                let results = {
                    let mut r;
                    // TODO: This loop is needed for asyncify. It will be refactored with https://github.com/wasmerio/wasmer/issues/3451
                    loop {

                        let store_mut = store.as_store_mut();
                        let context = store_mut.engine().0.context();

                        r = self.func.0.handle.function
                        .call(
                            &context,
                            JSValue::undefined(&context).to_object(&context),
                            &params_list,
                        );

                        // let store_mut = store.as_store_mut();
                        if let Some(callback) = store_mut.inner.on_called.take() {
                            match callback(store_mut) {
                                Ok(wasmer_types::OnCalledAction::InvokeAgain) => { continue; }
                                Ok(wasmer_types::OnCalledAction::Finish) => { break; }
                                Ok(wasmer_types::OnCalledAction::Trap(trap)) => { return Err(RuntimeError::user(trap)) },
                                Err(trap) => { return Err(RuntimeError::user(trap)) },
                            }
                        }
                        break;
                    }
                    r?
                };
                let mut rets_list_array = Rets::empty_array();
                let mut_rets = rets_list_array.as_mut() as *mut [RawValue] as *mut RawValue;
                let store_mut = store.as_store_mut();
                let context = store_mut.engine().0.context();
                match Rets::size() {
                    0 => {},
                    1 => unsafe {
                        let ty = Rets::wasm_types()[0];
                        let val = param_from_js(&context, &ty, &results);
                        *mut_rets = val.as_raw(&mut store);
                    }
                    _n => {
                        if !results.is_array(&context) {
                            panic!("Expected results to be an array.")
                        }
                        unimplemented!();
                        // let results = results.into();
                        // for (i, ret_type) in Rets::wasm_types().iter().enumerate() {
                        //     let ret = results.get(i as u32);
                        //     unsafe {
                        //         let val = param_from_js(&ret_type, &ret);
                        //         let slot = mut_rets.add(i);
                        //         *slot = val.as_raw(&mut store);
                        //     }
                        // }
                    }
                }
                Ok(unsafe { Rets::from_array(store, rets_list_array) })
            }
        }
    };
}

impl_native_traits!();
impl_native_traits!(A1);
impl_native_traits!(A1, A2);
impl_native_traits!(A1, A2, A3);
impl_native_traits!(A1, A2, A3, A4);
impl_native_traits!(A1, A2, A3, A4, A5);
impl_native_traits!(A1, A2, A3, A4, A5, A6);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_native_traits!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_native_traits!(
    A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18
);
impl_native_traits!(
    A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19
);
impl_native_traits!(
    A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20
);
