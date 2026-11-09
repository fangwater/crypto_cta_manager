use crate::auth::SourceAccess;

/// The gateway supplies this scope; one shared editor requires configure access
/// to every account it can affect, rather than one account's view permission.
pub(super) fn permitted(access: &SourceAccess, sources: &str, configure: bool) -> bool {
    let ids = sources.split(',').map(str::trim).collect::<Vec<_>>();
    !ids.is_empty()
        && ids.iter().all(|id| {
            !id.is_empty()
                && if configure {
                    access.configure.contains(*id)
                } else {
                    access.view.contains(*id)
                }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[test]
    fn shared_config_requires_every_configure_grant() {
        let mut access = SourceAccess {
            view: BTreeSet::from(["rx01".into(), "rx03".into(), "rx04".into()]),
            configure: BTreeSet::from(["rx01".into(), "rx03".into()]),
        };
        assert!(permitted(&access, "rx01,rx03,rx04", false));
        assert!(!permitted(&access, "rx01,rx03,rx04", true));
        access.configure.insert("rx04".into());
        assert!(permitted(&access, "rx01,rx03,rx04", true));
        assert!(!permitted(&access, "", true));
        assert!(!permitted(&access, "rx01,", true));
        assert!(!permitted(&access, "hidden", true));
    }
}
