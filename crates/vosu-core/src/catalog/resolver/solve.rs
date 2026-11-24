use super::{
    Candidate, Failure, MAX_CANDIDATES, MAX_NODES, MAX_SEARCH_WORK, RequirementSite, Snapshot,
};
use vosu_plugin_api::manifest::{Capability, PackageKind, Requirement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Catalog(usize),
    Installed(usize),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub consumer: Node,
    /// None is a built-in, deliberately not a reference-counted graph node.
    pub provider: Option<Node>,
    pub requirement: Requirement,
    pub optional: bool,
}
#[derive(Debug)]
pub struct Resolution {
    /// Candidate indices, in dependency-first order. Empty on every error.
    pub adds: Vec<usize>,
    pub edges: Vec<Edge>,
    pub errors: Vec<Failure>,
}
#[derive(Clone)]
pub(super) struct Task {
    pub consumer: Node,
    pub requirement: Requirement,
}
pub(super) struct Search<'s, 'a> {
    pub snapshot: &'s Snapshot<'a>,
    pub selected: Vec<Node>,
    pub edges: Vec<Edge>,
    tasks: Vec<Task>,
    ranked: Vec<usize>,
    eligible: Vec<Result<(), Failure>>,
    work: usize,
}
impl Node {
    pub fn id<'a>(self, snapshot: &Snapshot<'a>) -> &'a str {
        match self {
            Self::Catalog(i) => &snapshot.candidates[i].entry.id,
            Self::Installed(i) => snapshot.installed[i].manifest.fields().id,
        }
    }
    pub fn kind(self, snapshot: &Snapshot<'_>) -> PackageKind {
        match self {
            Self::Catalog(i) => snapshot.candidates[i].kind,
            Self::Installed(i) => snapshot.installed[i].manifest.kind(),
        }
    }
    pub(super) fn requirements<'a>(
        self,
        snapshot: &Snapshot<'a>,
        optional: bool,
    ) -> &'a [Capability] {
        match self {
            Self::Catalog(i) => {
                let entry = snapshot.candidates[i].entry;
                if optional {
                    &entry.optional
                } else {
                    &entry.requires
                }
            }
            Self::Installed(i) => {
                let fields = snapshot.installed[i].manifest.fields();
                if optional {
                    fields.optional
                } else {
                    fields.requires
                }
            }
        }
    }
    pub(super) fn satisfies(self, snapshot: &Snapshot<'_>, requirement: &Requirement) -> bool {
        match self {
            Self::Catalog(i) => snapshot.candidates[i]
                .entry
                .provides
                .iter()
                .any(|p| requirement.satisfied_by(&p.id, &p.version)),
            Self::Installed(i) => snapshot.installed[i]
                .manifest
                .fields()
                .provides
                .iter()
                .any(|p| requirement.satisfied_by(&p.id, &p.version)),
        }
    }
}
/// Requested identities explicitly update their installed counterparts. Every
/// other installed consumer is a pinned root, including dependency-only plugins.
pub fn resolve(snapshot: &Snapshot<'_>, requests: &[super::Request<'_>]) -> Resolution {
    let result = run(snapshot, requests);
    match result {
        Ok((adds, edges)) => Resolution {
            adds,
            edges,
            errors: Vec::new(),
        },
        Err(error) => Resolution {
            adds: Vec::new(),
            edges: Vec::new(),
            errors: vec![error],
        },
    }
}
fn run(
    snapshot: &Snapshot<'_>,
    requests: &[super::Request<'_>],
) -> Result<(Vec<usize>, Vec<Edge>), Failure> {
    if snapshot.candidates.len() > MAX_CANDIDATES
        || snapshot.installed.len() > MAX_NODES
        || requests.len() > MAX_NODES
    {
        return Err(Failure::ResolutionLimit);
    }
    let mut ranked: Vec<_> = (0..snapshot.candidates.len()).collect();
    ranked.sort_by_key(|&i| snapshot.candidates[i].rank());
    let eligible = snapshot
        .candidates
        .iter()
        .map(|c| c.eligibility(snapshot))
        .collect();
    let mut search = Search {
        snapshot,
        selected: Vec::new(),
        edges: Vec::new(),
        tasks: Vec::new(),
        ranked,
        eligible,
        work: 0,
    };
    for request in requests {
        let i = snapshot
            .candidates
            .iter()
            .position(|c| {
                c.kind == request.kind && c.entry.id == request.id && c.source == request.source
            })
            .ok_or(Failure::RequestedUnavailable)?;
        search.eligible[i].clone()?;
        search.select(Node::Catalog(i))?;
    }
    for (i, installed) in snapshot.installed.iter().enumerate() {
        let fields = installed.manifest.fields();
        if !requests
            .iter()
            .any(|r| r.kind == installed.manifest.kind() && r.id == fields.id)
        {
            search.select(Node::Installed(i))?;
        }
    }
    search.search(0)?;
    let order = search.order()?;
    Ok((order, search.edges))
}
impl Search<'_, '_> {
    pub(super) fn tick(&mut self) -> Result<(), Failure> {
        self.work += 1;
        if self.work > MAX_SEARCH_WORK {
            Err(Failure::ResolutionLimit)
        } else {
            Ok(())
        }
    }
    fn select(&mut self, node: Node) -> Result<(), Failure> {
        if self.selected.contains(&node) {
            return Ok(());
        }
        if self.selected.len() >= MAX_NODES {
            return Err(Failure::ResolutionLimit);
        }
        if self.selected.iter().any(|other| {
            other.kind(self.snapshot) == node.kind(self.snapshot)
                && other.id(self.snapshot) == node.id(self.snapshot)
        }) {
            return Err(Failure::InvalidInput);
        }
        self.selected.push(node);
        for cap in node.requirements(self.snapshot, false) {
            self.tick()?;
            if self.tasks.len() >= super::MAX_REQUIREMENTS {
                return Err(Failure::ResolutionLimit);
            }
            self.tasks.push(Task {
                consumer: node,
                requirement: Requirement::parse(&cap.id, &cap.version)
                    .map_err(|_| Failure::InvalidInput)?,
            });
        }
        Ok(())
    }
    fn search(&mut self, cursor: usize) -> Result<(), Failure> {
        let edges = self.edges.len();
        let result = self.search_inner(cursor);
        if result.is_err() {
            self.edges.truncate(edges);
        }
        result
    }
    fn search_inner(&mut self, mut cursor: usize) -> Result<(), Failure> {
        while cursor < self.tasks.len() {
            self.tick()?;
            let Task {
                consumer,
                requirement,
            } = self.tasks[cursor].clone();
            if self.snapshot.builtins.satisfies(&requirement) {
                self.edges.push(Edge {
                    consumer,
                    provider: None,
                    requirement,
                    optional: false,
                });
                cursor += 1;
                continue;
            }
            // Installed versions are pinned, not catalog alternatives. Their
            // requirements were already enqueued as kept roots.
            if let Some(provider) = self.selected.iter().copied().find(|n| {
                matches!(n, Node::Installed(_)) && n.satisfies(self.snapshot, &requirement)
            }) {
                self.edges.push(Edge {
                    consumer,
                    provider: Some(provider),
                    requirement,
                    optional: false,
                });
                cursor += 1;
                continue;
            }
            let mut failure = None;
            let mut conflicting = Vec::new();
            for rank in 0..self.ranked.len() {
                self.tick()?;
                let i = self.ranked[rank];
                let candidate: &Candidate<'_> = &self.snapshot.candidates[i];
                let provider = Node::Catalog(i);
                if candidate.kind != PackageKind::Plugin
                    || !provider.satisfies(self.snapshot, &requirement)
                {
                    continue;
                }
                if let Err(error) = &self.eligible[i] {
                    failure.get_or_insert_with(|| error.clone());
                    continue;
                }
                if let Some(assigned) = self.selected.iter().copied().find(|n| {
                    n.kind(self.snapshot) == PackageKind::Plugin
                        && n.id(self.snapshot) == provider.id(self.snapshot)
                        && *n != provider
                }) {
                    if !conflicting.contains(&assigned) {
                        conflicting.push(assigned);
                    }
                    continue;
                }
                let selected = self.selected.len();
                let tasks = self.tasks.len();
                let edges = self.edges.len();
                self.select(provider)?;
                self.edges.push(Edge {
                    consumer,
                    provider: Some(provider),
                    requirement: requirement.clone(),
                    optional: false,
                });
                match self.search(cursor + 1) {
                    Ok(()) => return Ok(()),
                    Err(Failure::ResolutionLimit) => return Err(Failure::ResolutionLimit),
                    Err(error) => {
                        failure = Some(error);
                    }
                }
                self.selected.truncate(selected);
                self.tasks.truncate(tasks);
                self.edges.truncate(edges);
            }
            if !conflicting.is_empty() {
                // Include kept consumers even when their tasks have not yet
                // been visited; diagnostics must not depend on frontier order.
                let mut sites: Vec<_> = self
                    .tasks
                    .iter()
                    .filter(|task| {
                        conflicting
                            .iter()
                            .any(|n| n.satisfies(self.snapshot, &task.requirement))
                    })
                    .map(|task| self.site(task.consumer, &task.requirement))
                    .collect();
                sites.push(self.site(consumer, &requirement));
                return Err(Failure::Conflict(sites));
            }
            return Err(
                failure.unwrap_or_else(|| Failure::Missing(self.site(consumer, &requirement)))
            );
        }
        // A complete required assignment must also have a valid dependency DAG.
        self.order()?;
        let edges = self.edges.len();
        self.resolve_optional()?;
        let result = self.order().map(|_| ());
        if result.is_err() {
            self.edges.truncate(edges);
        }
        result
    }
    pub(super) fn site(&self, consumer: Node, requirement: &Requirement) -> RequirementSite {
        RequirementSite {
            consumer: consumer.id(self.snapshot).to_owned(),
            capability: requirement.id.clone(),
            range: requirement.raw_version.clone(),
        }
    }
    pub(super) fn order(&mut self) -> Result<Vec<usize>, Failure> {
        let mut done = Vec::new();
        let mut stack = Vec::new();
        let mut path = Vec::new();
        let mut adds = Vec::new();
        for i in 0..self.selected.len() {
            self.visit(
                self.selected[i],
                &mut done,
                &mut stack,
                &mut path,
                &mut adds,
            )?;
        }
        Ok(adds)
    }
    fn visit(
        &mut self,
        node: Node,
        done: &mut Vec<Node>,
        stack: &mut Vec<Node>,
        path: &mut Vec<RequirementSite>,
        adds: &mut Vec<usize>,
    ) -> Result<(), Failure> {
        self.tick()?;
        if done.contains(&node) {
            return Ok(());
        }
        stack.push(node);
        for i in 0..self.edges.len() {
            self.tick()?;
            if self.edges[i].consumer != node {
                continue;
            }
            let Some(provider) = self.edges[i].provider else {
                continue;
            };
            path.push(self.site(node, &self.edges[i].requirement));
            if let Some(start) = stack.iter().position(|n| *n == provider) {
                return Err(Failure::Cycle(path[start..].to_vec()));
            }
            self.visit(provider, done, stack, path, adds)?;
            path.pop();
        }
        stack.pop();
        done.push(node);
        if let Node::Catalog(i) = node {
            adds.push(i);
        }
        Ok(())
    }
}
