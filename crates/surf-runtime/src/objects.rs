//! `NativeObject` wrappers exposed to scripts. Implemented in task 7.

use futures::future::LocalBoxFuture;
use surf_browser::Page;
use surf_vm::{Args, NativeObject, RuntimeError, Value, Vm};

/// `page`, `page(2)`, `work.page(1)`.
pub struct PageObject(pub Page);

impl NativeObject for PageObject {
    fn type_name(&self) -> &str {
        "page"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            let _ = args;
            Err(RuntimeError::new(format!(
                "page.{name}: not implemented yet (task 7) [page {}]",
                self.0.index()
            )))
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "index" => Some(Value::Int(self.0.index() as i64)),
            _ => None,
        }
    }
}
