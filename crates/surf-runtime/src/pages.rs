//! Implicit page resolution.

use std::cell::RefCell;
use surf_browser::Page;
use surf_vm::RuntimeError;

/// Pages of the default browser in creation order, plus user names.
#[derive(Default)]
pub struct PageRegistry {
    pages: RefCell<Vec<(Page, Option<String>)>>,
}

impl PageRegistry {
    /// Register a newly created page.
    pub fn add(&self, page: Page, name: Option<String>) {
        self.pages.borrow_mut().push((page, name));
    }

    /// Number of open pages.
    pub fn len(&self) -> usize {
        self.pages.borrow().len()
    }

    /// Whether no page is open.
    pub fn is_empty(&self) -> bool {
        self.pages.borrow().is_empty()
    }

    /// The sole page, or the "several pages are open" error. `None` when no
    /// page exists yet (caller auto-creates).
    pub fn sole(&self, action: &str) -> Result<Option<Page>, RuntimeError> {
        let pages = self.pages.borrow();
        match pages.len() {
            0 => Ok(None),
            1 => Ok(Some(pages[0].0.clone())),
            _ => {
                let names: Vec<String> = pages
                    .iter()
                    .enumerate()
                    .map(|(i, (_, name))| match name {
                        Some(n) => format!("{n:?}"),
                        None => (i + 1).to_string(),
                    })
                    .collect();
                Err(RuntimeError::new(format!(
                    "several pages are open ({}) — say which: page(2).{action}(…)",
                    names.join(", ")
                )))
            }
        }
    }

    /// `page(n)` by 1-based index.
    pub fn by_index(&self, n: usize) -> Option<Page> {
        self.pages
            .borrow()
            .get(n.checked_sub(1)?)
            .map(|(p, _)| p.clone())
    }

    /// `page("login")` by name.
    pub fn by_name(&self, name: &str) -> Option<Page> {
        self.pages
            .borrow()
            .iter()
            .find(|(_, n)| n.as_deref() == Some(name))
            .map(|(p, _)| p.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_sole_page() {
        let r = PageRegistry::default();
        assert!(r.sole("click").unwrap().is_none());
        assert!(r.by_index(1).is_none());
        assert!(r.by_name("login").is_none());
    }
}
