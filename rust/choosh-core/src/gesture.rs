//! Deterministic pointer-gesture ownership arbitration.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GestureConfig {
    pub edge_width: i32,
    pub navigation_threshold: i32,
    pub content_threshold: i32,
    pub max_trace_points: usize,
    pub max_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Surface {
    Terminal {
        mouse_reporting: bool,
        selection_active: bool,
    },
    Editor {
        selection_active: bool,
    },
    WebView {
        can_scroll_horizontally: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GestureOwner {
    Undecided,
    PageNavigation,
    TerminalMouse,
    TerminalSelection,
    TerminalScroll,
    EditorSelection,
    EditorScroll,
    WebViewScroll,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GestureError {
    InvalidConfig,
    AlreadyActive,
    NotActive,
    StaleGeneration,
    PointerMismatch,
    TraceLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GestureUpdate {
    Began { generation: u64 },
    Owned(GestureOwner),
    Ended(GestureOwner),
    Cancelled(GestureOwner),
}

#[derive(Clone, Copy, Debug)]
struct ActiveGesture {
    generation: u64,
    first_pointer: u32,
    pointers: usize,
    origin: Point,
    owner: GestureOwner,
    trace_points: usize,
    edge_origin: bool,
    surface: Surface,
}

#[derive(Debug)]
pub struct GestureArbiter {
    config: GestureConfig,
    generation: u64,
    active: Option<ActiveGesture>,
}

impl GestureArbiter {
    /// Creates an arbiter with injected pixel thresholds and trace bounds.
    ///
    /// # Errors
    ///
    /// Rejects negative/zero thresholds, zero trace capacity, or a zero maximum
    /// generation.
    pub fn new(config: GestureConfig) -> Result<Self, GestureError> {
        if config.edge_width < 0
            || config.navigation_threshold <= 0
            || config.content_threshold <= 0
            || config.max_trace_points == 0
            || config.max_generation == 0
        {
            return Err(GestureError::InvalidConfig);
        }
        Ok(Self {
            config,
            generation: 0,
            active: None,
        })
    }

    /// Starts one pointer sequence and allocates its bounded generation.
    ///
    /// # Errors
    ///
    /// Rejects overlapping gestures or generation exhaustion.
    pub fn begin(
        &mut self,
        pointer: u32,
        point: Point,
        surface_width: i32,
        surface: Surface,
    ) -> Result<GestureUpdate, GestureError> {
        if self.active.is_some() {
            return Err(GestureError::AlreadyActive);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(GestureError::StaleGeneration)?;
        if generation > self.config.max_generation {
            return Err(GestureError::StaleGeneration);
        }
        self.generation = generation;
        let right_edge = surface_width.saturating_sub(self.config.edge_width);
        self.active = Some(ActiveGesture {
            generation,
            first_pointer: pointer,
            pointers: 1,
            origin: point,
            owner: GestureOwner::Undecided,
            trace_points: 1,
            edge_origin: point.x <= self.config.edge_width || point.x >= right_edge,
            surface,
        });
        Ok(GestureUpdate::Began { generation })
    }

    /// Adds a pointer. Two pointers immediately claim terminal scrolling; other
    /// surfaces retain their content-scroll owner once movement crosses threshold.
    ///
    /// # Errors
    ///
    /// Rejects stale generations, inactive gestures, and trace exhaustion.
    pub fn pointer_added(
        &mut self,
        generation: u64,
        point: Point,
    ) -> Result<GestureUpdate, GestureError> {
        let trace_limit = self.config.max_trace_points;
        let active = self.active_mut(generation)?;
        add_trace(active, trace_limit)?;
        active.pointers = active.pointers.saturating_add(1);
        if active.owner == GestureOwner::Undecided
            && matches!(active.surface, Surface::Terminal { .. })
        {
            active.owner = GestureOwner::TerminalScroll;
        }
        let _ = point;
        Ok(GestureUpdate::Owned(active.owner))
    }

    /// Advances arbitration. Once an owner is chosen it is returned unchanged.
    ///
    /// # Errors
    ///
    /// Rejects stale generations, wrong primary pointers, and trace exhaustion.
    pub fn move_to(
        &mut self,
        generation: u64,
        pointer: u32,
        point: Point,
    ) -> Result<GestureUpdate, GestureError> {
        let navigation_threshold = self.config.navigation_threshold;
        let content_threshold = self.config.content_threshold;
        let trace_limit = self.config.max_trace_points;
        let active = self.active_mut(generation)?;
        if active.first_pointer != pointer {
            return Err(GestureError::PointerMismatch);
        }
        add_trace(active, trace_limit)?;
        if active.owner != GestureOwner::Undecided {
            return Ok(GestureUpdate::Owned(active.owner));
        }
        let dx = i64::from(point.x) - i64::from(active.origin.x);
        let dy = i64::from(point.y) - i64::from(active.origin.y);
        let horizontal = dx.unsigned_abs() > dy.unsigned_abs();
        let navigation_threshold = navigation_threshold.unsigned_abs().into();
        let content_threshold = content_threshold.unsigned_abs().into();
        if active.edge_origin && horizontal && dx.unsigned_abs() >= navigation_threshold {
            active.owner = GestureOwner::PageNavigation;
        } else if movement(dx, dy) >= content_threshold {
            active.owner = content_owner(active.surface, active.pointers, horizontal);
        }
        Ok(GestureUpdate::Owned(active.owner))
    }

    /// Ends the gesture only when the primary pointer lifts.
    ///
    /// # Errors
    ///
    /// Rejects inactive/stale sequences or a mismatched primary pointer.
    pub fn end(&mut self, generation: u64, pointer: u32) -> Result<GestureUpdate, GestureError> {
        let active = self.active.ok_or(GestureError::NotActive)?;
        if active.generation != generation {
            return Err(GestureError::StaleGeneration);
        }
        if active.first_pointer != pointer {
            return Err(GestureError::PointerMismatch);
        }
        self.active = None;
        Ok(GestureUpdate::Ended(active.owner))
    }

    /// Cancels and resets all pointer state.
    ///
    /// # Errors
    ///
    /// Rejects inactive or stale sequences.
    pub fn cancel(&mut self, generation: u64) -> Result<GestureUpdate, GestureError> {
        let active = self.active.ok_or(GestureError::NotActive)?;
        if active.generation != generation {
            return Err(GestureError::StaleGeneration);
        }
        self.active = None;
        Ok(GestureUpdate::Cancelled(active.owner))
    }

    fn active_mut(&mut self, generation: u64) -> Result<&mut ActiveGesture, GestureError> {
        let active = self.active.as_mut().ok_or(GestureError::NotActive)?;
        if active.generation == generation {
            Ok(active)
        } else {
            Err(GestureError::StaleGeneration)
        }
    }
}

fn add_trace(active: &mut ActiveGesture, maximum: usize) -> Result<(), GestureError> {
    if active.trace_points == maximum {
        return Err(GestureError::TraceLimit);
    }
    active.trace_points += 1;
    Ok(())
}

fn movement(dx: i64, dy: i64) -> u64 {
    dx.unsigned_abs().max(dy.unsigned_abs())
}

fn content_owner(surface: Surface, pointers: usize, horizontal: bool) -> GestureOwner {
    match surface {
        Surface::Terminal { .. } if pointers > 1 => GestureOwner::TerminalScroll,
        Surface::Terminal {
            mouse_reporting: true,
            ..
        } => GestureOwner::TerminalMouse,
        Surface::Terminal {
            selection_active: true,
            ..
        } => GestureOwner::TerminalSelection,
        Surface::Terminal { .. } => GestureOwner::TerminalScroll,
        Surface::Editor {
            selection_active: true,
        } => GestureOwner::EditorSelection,
        Surface::Editor { .. } => GestureOwner::EditorScroll,
        Surface::WebView {
            can_scroll_horizontally: true,
        } if horizontal => GestureOwner::WebViewScroll,
        Surface::WebView { .. } => GestureOwner::WebViewScroll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arbiter() -> GestureArbiter {
        GestureArbiter::new(GestureConfig {
            edge_width: 12,
            navigation_threshold: 20,
            content_threshold: 8,
            max_trace_points: 8,
            max_generation: 10,
        })
        .unwrap()
    }

    #[test]
    fn golden_precedence_table_is_stable() {
        let cases = [
            (
                Surface::Terminal {
                    mouse_reporting: true,
                    selection_active: false,
                },
                GestureOwner::TerminalMouse,
            ),
            (
                Surface::Terminal {
                    mouse_reporting: false,
                    selection_active: true,
                },
                GestureOwner::TerminalSelection,
            ),
            (
                Surface::Terminal {
                    mouse_reporting: false,
                    selection_active: false,
                },
                GestureOwner::TerminalScroll,
            ),
            (
                Surface::Editor {
                    selection_active: true,
                },
                GestureOwner::EditorSelection,
            ),
            (
                Surface::Editor {
                    selection_active: false,
                },
                GestureOwner::EditorScroll,
            ),
            (
                Surface::WebView {
                    can_scroll_horizontally: true,
                },
                GestureOwner::WebViewScroll,
            ),
        ];
        for (surface, expected) in cases {
            let mut arbiter = arbiter();
            arbiter
                .begin(1, Point { x: 50, y: 50 }, 100, surface)
                .unwrap();
            assert_eq!(
                arbiter.move_to(1, 1, Point { x: 60, y: 50 }).unwrap(),
                GestureUpdate::Owned(expected)
            );
        }
    }

    #[test]
    fn edge_navigation_claims_before_content_and_never_transfers() {
        let mut arbiter = arbiter();
        arbiter
            .begin(
                4,
                Point { x: 2, y: 10 },
                100,
                Surface::Terminal {
                    mouse_reporting: true,
                    selection_active: false,
                },
            )
            .unwrap();
        assert_eq!(
            arbiter.move_to(1, 4, Point { x: 23, y: 11 }).unwrap(),
            GestureUpdate::Owned(GestureOwner::PageNavigation)
        );
        assert_eq!(
            arbiter.move_to(1, 4, Point { x: 23, y: 80 }).unwrap(),
            GestureUpdate::Owned(GestureOwner::PageNavigation)
        );
    }

    #[test]
    fn interior_content_claim_cannot_later_become_navigation() {
        let mut arbiter = arbiter();
        arbiter
            .begin(
                1,
                Point { x: 50, y: 10 },
                100,
                Surface::Editor {
                    selection_active: true,
                },
            )
            .unwrap();
        arbiter.move_to(1, 1, Point { x: 50, y: 20 }).unwrap();
        assert_eq!(
            arbiter.move_to(1, 1, Point { x: 99, y: 20 }).unwrap(),
            GestureUpdate::Owned(GestureOwner::EditorSelection)
        );
    }

    #[test]
    fn second_pointer_claims_terminal_scroll_and_cancel_resets() {
        let mut arbiter = arbiter();
        arbiter
            .begin(
                7,
                Point { x: 40, y: 40 },
                100,
                Surface::Terminal {
                    mouse_reporting: true,
                    selection_active: false,
                },
            )
            .unwrap();
        assert_eq!(
            arbiter.pointer_added(1, Point { x: 45, y: 40 }).unwrap(),
            GestureUpdate::Owned(GestureOwner::TerminalScroll)
        );
        assert_eq!(
            arbiter.cancel(1).unwrap(),
            GestureUpdate::Cancelled(GestureOwner::TerminalScroll)
        );
        assert!(matches!(
            arbiter.begin(
                8,
                Point { x: 1, y: 1 },
                100,
                Surface::WebView {
                    can_scroll_horizontally: true
                }
            ),
            Ok(GestureUpdate::Began { generation: 2 })
        ));
    }

    #[test]
    fn stale_pointer_and_trace_overflow_fail_without_stealing() {
        let mut arbiter = GestureArbiter::new(GestureConfig {
            edge_width: 1,
            navigation_threshold: 4,
            content_threshold: 2,
            max_trace_points: 2,
            max_generation: 2,
        })
        .unwrap();
        arbiter
            .begin(
                1,
                Point { x: 5, y: 5 },
                10,
                Surface::Editor {
                    selection_active: false,
                },
            )
            .unwrap();
        assert_eq!(
            arbiter.move_to(9, 1, Point { x: 6, y: 5 }),
            Err(GestureError::StaleGeneration)
        );
        assert_eq!(
            arbiter.move_to(1, 2, Point { x: 6, y: 5 }),
            Err(GestureError::PointerMismatch)
        );
        arbiter.move_to(1, 1, Point { x: 7, y: 5 }).unwrap();
        assert_eq!(
            arbiter.move_to(1, 1, Point { x: 8, y: 5 }),
            Err(GestureError::TraceLimit)
        );
        assert_eq!(
            arbiter.end(1, 1).unwrap(),
            GestureUpdate::Ended(GestureOwner::EditorScroll)
        );
    }
}
