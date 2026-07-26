//! Headless, deterministic annotation navigation state.

use crate::annotation::{AnchorStatus, Annotation, AnnotationId, AnnotationRegistry, WorkspaceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationView {
    pub annotation_id: AnnotationId,
    pub document: String,
    pub body_markdown: String,
    pub status: AnchorStatus,
    pub selected: bool,
}

/// Projects durable records into a bounded navigation list. Selection is local
/// view state and never changes the durable registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationNavigator {
    workspace: WorkspaceId,
    selected: Option<AnnotationId>,
}

impl AnnotationNavigator {
    #[must_use]
    pub fn new(workspace: WorkspaceId) -> Self {
        Self {
            workspace,
            selected: None,
        }
    }

    pub fn select(&mut self, id: Option<AnnotationId>) {
        self.selected = id;
    }

    #[must_use]
    pub fn project(&self, registry: &AnnotationRegistry) -> Vec<AnnotationView> {
        registry
            .iter()
            .filter(|a| a.key.workspace_id == self.workspace)
            .map(|a| view(a, self.selected.as_ref()))
            .collect()
    }
}

fn view(a: &Annotation, selected: Option<&AnnotationId>) -> AnnotationView {
    AnnotationView {
        annotation_id: a.key.annotation_id.clone(),
        document: a.key.document.as_str().to_owned(),
        body_markdown: a.body_markdown.clone(),
        status: a.status.clone(),
        selected: selected == Some(&a.key.annotation_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{
        AnnotationKey, AnnotationLimits, ContextFingerprint, DocumentIdentity, HostId,
        NewAnnotation, TextRange,
    };

    fn id(v: &str) -> AnnotationId {
        AnnotationId::parse(v).unwrap()
    }
    fn registry() -> AnnotationRegistry {
        AnnotationRegistry::new(AnnotationLimits::new(8, 128, 4, 32).unwrap())
    }
    fn add(r: &mut AnnotationRegistry, idv: &str, doc: &str, ws: &str) {
        r.create(NewAnnotation {
            key: AnnotationKey {
                host_id: HostId::parse("h").unwrap(),
                workspace_id: WorkspaceId::parse(ws).unwrap(),
                document: DocumentIdentity::parse(doc).unwrap(),
                annotation_id: id(idv),
            },
            document_revision: 1,
            range: TextRange { start: 0, end: 1 },
            context: ContextFingerprint {
                selected_digest: [1; 32],
                prefix: String::new(),
                suffix: String::new(),
            },
            body_markdown: "note".into(),
        })
        .unwrap();
    }

    #[test]
    fn projection_is_stable_and_workspace_scoped() {
        let mut r = registry();
        add(&mut r, "b", "b.md", "w");
        add(&mut r, "a", "a.md", "w");
        add(&mut r, "x", "x.md", "other");
        let n = AnnotationNavigator::new(WorkspaceId::parse("w").unwrap());
        let v = n.project(&r);
        assert_eq!(
            v.iter()
                .map(|x| x.annotation_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(v.iter().all(|x| !x.selected));
    }

    #[test]
    fn selection_is_view_only_and_exact_id() {
        let mut r = registry();
        add(&mut r, "a", "a.md", "w");
        add(&mut r, "ab", "b.md", "w");
        let mut n = AnnotationNavigator::new(WorkspaceId::parse("w").unwrap());
        n.select(Some(id("a")));
        let v = n.project(&r);
        assert!(v[0].selected);
        assert!(!v[1].selected);
        assert_eq!(r.iter().count(), 2);
    }
}
