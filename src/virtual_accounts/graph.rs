//! Resolve nested Virtual configuration into stable leaf publication bindings.
use super::*;

pub(super) const MAX_DEPTH: usize = 64;

#[derive(Clone, Default)]
pub(super) struct Definition {
    pub bindings: Vec<VirtualBinding>,
    pub children: Vec<AccountFollow>,
}

pub(super) type Definitions = BTreeMap<String, Definition>;

impl Definition {
    pub(super) fn replace(&mut self, request: &SaveVirtualAccount) {
        self.bindings = request.bindings.clone();
        if let Some(children) = &request.children {
            self.children = children.clone();
        }
    }
}

pub(super) fn merge(
    combined: &mut BTreeMap<String, VirtualBinding>,
    binding: SaveBindingRequest,
) -> Result<()> {
    if let Some(existing) = combined.get_mut(&binding.binding_name) {
        ensure!(
            existing.position_strategy_name == binding.position_strategy_name
                && existing.order_strategy_name == binding.order_strategy_name,
            "binding {} conflicts across virtual accounts: position strategy and order template must match",
            binding.binding_name
        );
        existing.shares += binding.shares;
        validate_nonnegative_multiplier(existing.shares, "combined shares")
            .map_err(anyhow::Error::msg)?;
    } else {
        combined.insert(
            binding.binding_name.clone(),
            VirtualBinding {
                binding_name: binding.binding_name,
                position_strategy_name: binding.position_strategy_name,
                order_strategy_name: binding.order_strategy_name,
                shares: binding.shares,
            },
        );
    }
    Ok(())
}

fn resolve(
    id: &str,
    definitions: &Definitions,
    visiting: &mut BTreeSet<String>,
    resolved: &mut BTreeMap<String, (Vec<VirtualBinding>, usize)>,
) -> Result<(Vec<VirtualBinding>, usize)> {
    ensure!(!visiting.contains(id), "Virtual reference cycle detected");
    if let Some(result) = resolved.get(id) {
        return Ok(result.clone());
    }
    ensure!(
        visiting.len() < MAX_DEPTH,
        "Virtual nesting exceeds {MAX_DEPTH} levels"
    );
    let definition = definitions
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("unknown virtual account: {id}"))?;
    visiting.insert(id.to_owned());
    let mut combined = BTreeMap::new();
    for binding in effective_bindings(&definition.bindings, 1.0)? {
        merge(&mut combined, binding)?;
    }
    let mut children = definition.children.clone();
    children.sort_by(|a, b| a.virtual_id.cmp(&b.virtual_id));
    let mut previous = None;
    let mut depth = 1;
    for child in children {
        ensure!(
            previous.as_ref() != Some(&child.virtual_id),
            "duplicate child virtual account: {}",
            child.virtual_id
        );
        previous = Some(child.virtual_id.clone());
        validate_nonnegative_multiplier(child.multiplier, "child multiplier")
            .map_err(anyhow::Error::msg)?;
        let (bindings, child_depth) = resolve(&child.virtual_id, definitions, visiting, resolved)?;
        depth = depth.max(child_depth + 1);
        ensure!(
            depth <= MAX_DEPTH,
            "Virtual nesting exceeds {MAX_DEPTH} levels"
        );
        for binding in effective_bindings(&bindings, child.multiplier)? {
            merge(&mut combined, binding)?;
        }
    }
    visiting.remove(id);
    let result = (combined.into_values().collect::<Vec<_>>(), depth);
    resolved.insert(id.to_owned(), result.clone());
    Ok(result)
}

pub(super) fn expand(definitions: &Definitions) -> Result<BTreeMap<String, Vec<VirtualBinding>>> {
    let mut resolved = BTreeMap::new();
    for id in definitions.keys() {
        resolve(id, definitions, &mut BTreeSet::new(), &mut resolved)?;
    }
    Ok(resolved
        .into_iter()
        .map(|(id, (bindings, _))| (id, bindings))
        .collect())
}

pub(super) async fn load(conn: &mut PgConnection) -> Result<Definitions> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT virtual_id FROM cta_virtual_accounts ORDER BY virtual_id")
            .fetch_all(&mut *conn)
            .await?;
    let mut definitions: Definitions = ids
        .into_iter()
        .map(|id| (id, Definition::default()))
        .collect();
    for row in sqlx::query("SELECT virtual_id, binding_name, position_strategy_name, order_strategy_name, shares FROM cta_virtual_account_bindings ORDER BY virtual_id, binding_name")
        .fetch_all(&mut *conn).await? {
        let id: String = row.try_get("virtual_id")?;
        definitions.get_mut(&id).ok_or_else(|| anyhow::anyhow!("unknown virtual account: {id}"))?.bindings.push(VirtualBinding {
            binding_name: row.try_get("binding_name")?,
            position_strategy_name: row.try_get("position_strategy_name")?,
            order_strategy_name: row.try_get("order_strategy_name")?,
            shares: row.try_get("shares")?,
        });
    }
    for row in sqlx::query("SELECT parent_virtual_id, child_virtual_id, multiplier FROM cta_virtual_account_children ORDER BY parent_virtual_id, child_virtual_id")
        .fetch_all(&mut *conn).await? {
        let id: String = row.try_get("parent_virtual_id")?;
        definitions.get_mut(&id).ok_or_else(|| anyhow::anyhow!("unknown virtual account: {id}"))?.children.push(AccountFollow {
            virtual_id: row.try_get("child_virtual_id")?, multiplier: row.try_get("multiplier")?,
        });
    }
    Ok(definitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(shares: f64) -> VirtualBinding {
        VirtualBinding {
            binding_name: "signal".into(),
            position_strategy_name: "position".into(),
            order_strategy_name: "order".into(),
            shares,
        }
    }
    fn child(id: &str, multiplier: f64) -> AccountFollow {
        AccountFollow {
            virtual_id: id.into(),
            multiplier,
        }
    }
    fn definition(bindings: Vec<VirtualBinding>, children: Vec<AccountFollow>) -> Definition {
        Definition { bindings, children }
    }

    #[test]
    fn nested_ratios_multiply_shared_paths_sum_and_leaf_identity_survives() {
        let mut graph = Definitions::from([
            ("a".into(), definition(vec![leaf(2.0)], vec![])),
            (
                "b".into(),
                definition(vec![leaf(1.0)], vec![child("a", 3.0)]),
            ),
            (
                "root".into(),
                definition(vec![], vec![child("a", 0.5), child("b", 4.0)]),
            ),
        ]);
        let expanded = expand(&graph).unwrap();
        assert_eq!(expanded["root"], vec![leaf(29.0)]);
        assert_eq!(
            effective_bindings(&expanded["root"], 2.0).unwrap()[0].shares,
            58.0
        );
        graph.get_mut("root").unwrap().children.reverse();
        assert_eq!(expand(&graph).unwrap(), expanded);
        graph.get_mut("root").unwrap().children[0].multiplier = 0.0;
        assert_eq!(expand(&graph).unwrap()["root"], vec![leaf(1.0)]);
    }

    #[test]
    fn cycles_unknown_children_invalid_ratios_and_identity_conflicts_reject_the_graph() {
        let base = Definitions::from([
            ("a".into(), definition(vec![leaf(2.0)], vec![])),
            ("root".into(), definition(vec![], vec![child("a", 1.0)])),
        ]);
        for multiplier in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            let mut graph = base.clone();
            graph.get_mut("root").unwrap().children[0].multiplier = multiplier;
            assert!(expand(&graph).is_err());
        }
        let mut graph = base.clone();
        graph
            .get_mut("a")
            .unwrap()
            .children
            .push(child("root", 0.0));
        assert!(expand(&graph).unwrap_err().to_string().contains("cycle"));
        let mut graph = base.clone();
        graph
            .get_mut("root")
            .unwrap()
            .children
            .push(child("a", 2.0));
        assert!(expand(&graph).is_err());
        let mut graph = base.clone();
        graph.get_mut("root").unwrap().children[0].virtual_id = "missing".into();
        assert!(expand(&graph).is_err());
        let mut graph = base;
        let mut conflict = leaf(1.0);
        conflict.order_strategy_name = "different".into();
        graph.get_mut("root").unwrap().bindings.push(conflict);
        assert!(expand(&graph).is_err());
    }

    #[test]
    fn deeply_shared_subgraphs_cannot_bypass_the_depth_limit() {
        let mut graph = Definitions::new();
        for n in 0..MAX_DEPTH {
            graph.insert(
                format!("v{n:03}"),
                definition(
                    vec![],
                    if n == 0 {
                        vec![]
                    } else {
                        vec![child(&format!("v{:03}", n - 1), 1.0)]
                    },
                ),
            );
        }
        assert!(expand(&graph).is_ok());
        graph.insert(
            "zz-root".into(),
            definition(vec![], vec![child(&format!("v{:03}", MAX_DEPTH - 1), 0.0)]),
        );
        assert!(expand(&graph).is_err());
    }
}
