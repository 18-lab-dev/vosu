use super::{Edge, Failure, Node, solve::Search};
use vosu_plugin_api::manifest::Requirement;

impl Search<'_, '_> {
    /// The required search has already reached closure. Its available provider
    /// set cannot grow during this pass, so one complete pass is the optional
    /// fixed point: adding a grant never installs or activates another provider.
    pub(super) fn resolve_optional(&mut self) -> Result<(), Failure> {
        for consumer_index in 0..self.selected.len() {
            let consumer = self.selected[consumer_index];
            for capability in consumer.requirements(self.snapshot, true) {
                self.tick()?;
                let requirement = Requirement::parse(&capability.id, &capability.version)
                    .map_err(|_| Failure::InvalidInput)?;
                let provider = if self.snapshot.builtins.satisfies(&requirement) {
                    None
                } else {
                    let mut provider = None;
                    // Installed providers remain ahead of catalog alternatives.
                    for node in self
                        .selected
                        .iter()
                        .copied()
                        .filter(|n| matches!(n, Node::Installed(_)))
                    {
                        if node.satisfies(self.snapshot, &requirement) {
                            provider = Some(node);
                            break;
                        }
                    }
                    if provider.is_none() {
                        provider = self
                            .selected
                            .iter()
                            .copied()
                            .filter(|n| {
                                matches!(n, Node::Catalog(_))
                                    && n.satisfies(self.snapshot, &requirement)
                            })
                            .min_by_key(|n| match n {
                                Node::Catalog(i) => self.snapshot.candidates[*i].rank(),
                                Node::Installed(_) => unreachable!(),
                            });
                    }
                    let Some(provider) = provider else {
                        continue;
                    };
                    Some(provider)
                };
                self.edges.push(Edge {
                    consumer,
                    provider,
                    requirement,
                    optional: true,
                });
            }
        }
        Ok(())
    }
}
