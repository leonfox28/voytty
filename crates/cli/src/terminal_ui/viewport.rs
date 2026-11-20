//! Live, history and resume states for the desktop viewport.

use super::*;

pub(super) enum ViewportEffect {
    None,
    Render,
    RequestHistoryWindow(TerminalHistoryWindowQuery),
    RenderAndRequestHistoryWindow(TerminalHistoryWindowQuery),
    Resume,
}

#[derive(Default)]
pub(super) struct ViewportPresentationPacer {
    last_presented: Option<Instant>,
    pub(super) dirty: bool,
    pub(super) pending_deadline: Option<Instant>,
}

impl ViewportPresentationPacer {
    pub(super) fn mark_dirty(&mut self, now: Instant) {
        if self.dirty {
            return;
        }
        self.dirty = true;
        let earliest = self
            .last_presented
            .and_then(|last| last.checked_add(MIN_VIEWPORT_PRESENT_INTERVAL));
        self.pending_deadline = Some(earliest.filter(|deadline| *deadline > now).unwrap_or(now));
    }

    pub(super) const fn deadline(&self) -> Option<Instant> {
        self.pending_deadline
    }

    pub(super) fn due(&self, now: Instant) -> bool {
        self.dirty
            && self
                .pending_deadline
                .is_some_and(|deadline| now >= deadline)
    }

    pub(super) fn mark_presented(&mut self, now: Instant) {
        self.last_presented = Some(now);
        self.dirty = false;
        self.pending_deadline = None;
    }

    pub(super) fn cancel(&mut self) {
        self.dirty = false;
        self.pending_deadline = None;
    }
}

pub(super) struct HistoryViewport {
    pub(super) notice: Option<&'static str>,
}

pub(super) enum ViewportState {
    Live,
    History(HistoryViewport),
    ResumePending {
        retained_input: Vec<u8>,
        snapshot_applied: bool,
        presented_scroll_metrics: Option<TerminalScrollMetrics>,
    },
}

pub(super) struct ViewportController {
    pub(super) state: ViewportState,
    pub(super) content_size: TerminalSize,
    pub(super) live_metrics: Option<TerminalScrollMetrics>,
    pub(super) gutter_column: Option<u16>,
    pub(super) drag_grab_row: Option<u16>,
    pub(super) drag_last_request: Option<Instant>,
    pub(super) drag_deferred_target: Option<u64>,
    // Seeded for the initial live frame; unlike the cache's latest locally
    // presentable target, it subsequently advances only after a complete
    // outer-terminal transaction succeeds.
    pub(super) last_presented_scroll_metrics: Option<TerminalScrollMetrics>,
    // This is the physical column committed by the same successful outer
    // transaction, not merely the most recently requested layout.
    pub(super) presented_gutter_column: Option<u16>,
    pub(super) discard_window_response: bool,
    pub(super) window_cache: ViewportCache<TerminalSurfaceRow>,
}

#[derive(Clone, Copy)]
pub(super) struct ViewportDeltaPlan {
    pub(super) content_size: TerminalSize,
    pub(super) gutter_column: Option<u16>,
    pub(super) live_metrics: Option<TerminalScrollMetrics>,
    update_live_metrics: bool,
    content_size_changed: bool,
    anchor_observation: Option<ViewportAnchorObservation>,
    pub(super) selection_source: Option<SelectionSourceIdentity>,
}

impl ViewportController {
    pub(super) fn with_layout(
        layout: ChromeLayout,
        live_metrics: Option<TerminalScrollMetrics>,
    ) -> Self {
        let mut controller = Self {
            state: ViewportState::Live,
            content_size: layout.child,
            live_metrics,
            gutter_column: layout.gutter_column,
            drag_grab_row: None,
            drag_last_request: None,
            drag_deferred_target: None,
            last_presented_scroll_metrics: live_metrics,
            presented_gutter_column: None,
            discard_window_response: false,
            window_cache: ViewportCache::new(),
        };
        controller.observe_window_anchor(live_metrics);
        controller
    }

    pub(super) const fn content_size(&self) -> TerminalSize {
        self.content_size
    }

    pub(super) const fn is_live(&self) -> bool {
        matches!(&self.state, ViewportState::Live)
    }

    pub(super) const fn is_history(&self) -> bool {
        matches!(&self.state, ViewportState::History(_))
    }

    pub(super) fn has_complete_cached_viewport(&self) -> bool {
        self.is_history() && self.window_cache.visible_rows().is_some()
    }

    pub(super) fn has_complete_presentable_view(&self) -> bool {
        self.is_live() || self.has_complete_cached_viewport()
    }

    pub(super) const fn is_resume_pending(&self) -> bool {
        matches!(&self.state, ViewportState::ResumePending { .. })
    }

    pub(super) fn resize(&mut self, content_size: TerminalSize) {
        let changed = self.content_size != content_size;
        self.content_size = content_size;
        if changed {
            self.last_presented_scroll_metrics = None;
            if let ViewportState::ResumePending {
                presented_scroll_metrics,
                ..
            } = &mut self.state
            {
                // The old thumb geometry belongs to the previous viewport.
                // Wait for the replacement snapshot instead of projecting it
                // onto a size it never described.
                *presented_scroll_metrics = None;
            }
            self.observe_window_anchor(self.live_metrics);
        }
    }

    pub(super) fn set_layout(&mut self, layout: ChromeLayout) {
        self.resize(layout.child);
        self.gutter_column = layout.gutter_column;
        if self.gutter_column.is_none() {
            self.drag_grab_row = None;
            self.drag_last_request = None;
            self.drag_deferred_target = None;
        }
    }

    fn observe_window_anchor(&mut self, metrics: Option<TerminalScrollMetrics>) {
        let Some(metrics) = metrics.filter(|metrics| metrics.is_valid()) else {
            return;
        };
        let _ = self
            .window_cache
            .observe_anchor(TerminalHistoryWindowAnchor {
                epoch: metrics.epoch,
                revision: metrics.revision,
                max_offset_from_bottom: metrics.max_offset_from_bottom,
                viewport: self.content_size,
            });
    }

    pub(super) fn prefetch_live(&mut self) -> Option<TerminalHistoryWindowQuery> {
        if !self.is_live()
            || self.discard_window_response
            || self
                .live_metrics
                .is_none_or(|metrics| metrics.max_offset_from_bottom == 0)
        {
            return None;
        }
        self.window_cache.set_target(0).request
    }

    pub(super) fn refetch_history_window(&mut self) -> Option<TerminalHistoryWindowQuery> {
        if !self.is_history()
            || self.discard_window_response
            || self.window_cache.visible_rows().is_some()
        {
            return None;
        }
        self.window_cache
            .set_target(self.window_cache.desired_offset_from_bottom())
            .request
    }

    fn set_history_target(&mut self, target: u64) -> ViewportCacheUpdate {
        let mut update = self.window_cache.set_target(target);
        if self.discard_window_response && update.request.take().is_some() {
            // Invalidation retired the cache's correlation, not the
            // client's outstanding request. Keep only the desired target
            // until that obsolete response has actually been consumed.
            self.window_cache.defer_pending_request();
        }
        update
    }

    pub(super) fn effective_screen(&self, authoritative: ActiveScreen) -> ActiveScreen {
        if self.is_history() {
            ActiveScreen::Main
        } else {
            authoritative
        }
    }

    pub(super) fn outside_layout(&self, mouse: &SgrMouse) -> bool {
        mouse.row > self.content_size.rows
            || mouse.column > self.gutter_column.unwrap_or(self.content_size.columns)
    }

    pub(super) const fn gutter_drag_active(&self) -> bool {
        self.drag_grab_row.is_some()
    }

    pub(super) fn gutter_hit(&self, mouse: &SgrMouse) -> bool {
        Some(mouse.column) == self.gutter_column && mouse.row <= self.content_size.rows
    }

    pub(super) fn handle_gutter_mouse(
        &mut self,
        mouse: &SgrMouse,
        allow_live_navigation: bool,
    ) -> Option<ViewportEffect> {
        self.handle_gutter_mouse_at(mouse, allow_live_navigation, Instant::now())
    }

    fn handle_gutter_mouse_at(
        &mut self,
        mouse: &SgrMouse,
        allow_live_navigation: bool,
        now: Instant,
    ) -> Option<ViewportEffect> {
        if let Some(grab) = self.drag_grab_row {
            if mouse.release {
                self.drag_grab_row = None;
                self.drag_last_request = None;
                if let Some(target) = self.drag_deferred_target.take() {
                    self.window_cache.defer_pending_request();
                    return Some(self.scroll_to_offset(target));
                }
                if self.is_history() && self.window_cache.desired_offset_from_bottom() == 0 {
                    return Some(self.start_resume(Vec::new()));
                }
                return Some(ViewportEffect::None);
            }
            if mouse.is_motion() {
                if self.is_live() && !allow_live_navigation {
                    return Some(ViewportEffect::None);
                }
                let Some(metrics) = self.scroll_metrics() else {
                    self.drag_grab_row = None;
                    return Some(ViewportEffect::None);
                };
                let Some(geometry) = ScrollbarGeometry::new(self.content_size.rows, metrics) else {
                    self.drag_grab_row = None;
                    return Some(ViewportEffect::None);
                };
                let row = mouse.row.clamp(1, self.content_size.rows);
                let target = geometry.offset_for_pointer(row, grab, metrics.max_offset_from_bottom);
                if self.is_history() && target == self.window_cache.desired_offset_from_bottom() {
                    return Some(ViewportEffect::None);
                }
                let effect = self.scroll_to_offset(target);
                return Some(self.pace_drag_effect(effect, target, now));
            }
            self.drag_grab_row = None;
            self.drag_last_request = None;
            self.drag_deferred_target = None;
        }
        if Some(mouse.column) != self.gutter_column || mouse.row > self.content_size.rows {
            return None;
        }
        if self.is_live() && !allow_live_navigation {
            return Some(ViewportEffect::None);
        }
        if mouse.is_wheel() {
            return Some(self.navigate(mouse.wheel_is_up(), 1));
        }
        if mouse.release {
            self.drag_grab_row = None;
            self.drag_last_request = None;
            self.drag_deferred_target = None;
            return Some(ViewportEffect::None);
        }
        let Some(metrics) = self.scroll_metrics() else {
            return Some(ViewportEffect::None);
        };
        let Some(geometry) = ScrollbarGeometry::new(self.content_size.rows, metrics) else {
            return Some(ViewportEffect::None);
        };
        if mouse.is_left_press() {
            let grab = if geometry.contains_thumb(mouse.row) {
                geometry.grab_row(mouse.row)
            } else {
                geometry.thumb_len / 2
            };
            self.drag_grab_row = Some(grab);
            let target =
                geometry.offset_for_pointer(mouse.row, grab, metrics.max_offset_from_bottom);
            let effect = self.scroll_to_offset(target);
            if matches!(
                &effect,
                ViewportEffect::RequestHistoryWindow(_)
                    | ViewportEffect::RenderAndRequestHistoryWindow(_)
            ) {
                self.drag_last_request = Some(now);
            }
            return Some(effect);
        }
        Some(ViewportEffect::None)
    }

    fn pace_drag_effect(
        &mut self,
        effect: ViewportEffect,
        target: u64,
        now: Instant,
    ) -> ViewportEffect {
        let is_window_request = matches!(
            &effect,
            ViewportEffect::RequestHistoryWindow(_)
                | ViewportEffect::RenderAndRequestHistoryWindow(_)
        );
        if !is_window_request {
            return effect;
        }
        if self
            .drag_last_request
            .is_some_and(|last| now.saturating_duration_since(last) < DRAG_REQUEST_INTERVAL)
        {
            self.window_cache.defer_pending_request();
            self.drag_deferred_target = Some(target);
            return if matches!(&effect, ViewportEffect::RenderAndRequestHistoryWindow(_)) {
                ViewportEffect::Render
            } else {
                ViewportEffect::None
            };
        }
        self.drag_last_request = Some(now);
        self.drag_deferred_target = None;
        effect
    }

    pub(super) fn navigate(&mut self, older: bool, amount: usize) -> ViewportEffect {
        if matches!(self.state, ViewportState::Live) {
            if !older {
                return ViewportEffect::None;
            }
            if self
                .live_metrics
                .is_some_and(|metrics| metrics.max_offset_from_bottom == 0)
            {
                return ViewportEffect::None;
            }
            let Some(anchor) = self.window_cache.anchor() else {
                return ViewportEffect::None;
            };
            let target = u64::try_from(amount)
                .unwrap_or(u64::MAX)
                .min(anchor.max_offset_from_bottom);
            let update = self.set_history_target(target);
            self.state = ViewportState::History(HistoryViewport { notice: None });
            return history_window_effect(update);
        }
        let ViewportState::History(_) = &self.state else {
            return ViewportEffect::None;
        };
        let current = self.window_cache.desired_offset_from_bottom();
        let amount = u64::try_from(amount).unwrap_or(u64::MAX);
        let maximum = self
            .window_cache
            .anchor()
            .map_or(current, |anchor| anchor.max_offset_from_bottom);
        let target = if older {
            current.saturating_add(amount).min(maximum)
        } else {
            current.saturating_sub(amount)
        };
        if target == 0 {
            return self.start_resume(Vec::new());
        }
        history_window_effect(self.set_history_target(target))
    }

    pub(super) fn apply_view_history_window(
        &mut self,
        result: TerminalViewHistoryWindow,
    ) -> Result<ViewportEffect, CliError> {
        if self.discard_window_response {
            self.discard_window_response = false;
            // The client has consumed the old reply and released its one
            // request slot. Refill the latest desired view now; a gesture
            // made while waiting must not need to be repeated.
            return Ok(self
                .refetch_history_window()
                .or_else(|| self.prefetch_live())
                .map_or(ViewportEffect::None, ViewportEffect::RequestHistoryWindow));
        }
        if !self.window_cache.request_pending() {
            return Err(terminal_daemon_error(
                DomainErrorKind::MalformedFrame,
                "terminal history window arrived without a pending request",
            ));
        }
        match result {
            TerminalSurfaceHistoryWindowResult::Frame(frame) => {
                if frame.anchor.viewport != self.content_size {
                    self.window_cache.defer_pending_request();
                    let update = self
                        .window_cache
                        .set_target(self.window_cache.desired_offset_from_bottom());
                    return Ok(history_window_effect(update));
                }
                let rebased = frame.disposition == TerminalViewportDisposition::Rebased;
                let installed = self
                    .window_cache
                    .install_window(CachedViewportWindow {
                        disposition: frame.disposition,
                        anchor: frame.anchor,
                        target_offset_from_bottom: frame.target_offset_from_bottom,
                        first_row_from_live_top: frame.first_row_from_live_top,
                        rows: frame.rows,
                    })
                    .map_err(|_| {
                        terminal_daemon_error(
                            DomainErrorKind::MalformedFrame,
                            "terminal history window failed cache validation",
                        )
                    })?;
                if installed.render_local {
                    if self.window_cache.desired_offset_from_bottom() == 0 && self.is_live() {
                        return Ok(ViewportEffect::None);
                    }
                    if let ViewportState::History(history) = &mut self.state {
                        history.notice = rebased
                            .then_some("[voytty: retained history changed; showing closest view]");
                    }
                }
                Ok(match (installed.render_local, installed.request) {
                    (true, Some(query)) => ViewportEffect::RenderAndRequestHistoryWindow(query),
                    (true, None) => ViewportEffect::Render,
                    (false, Some(query)) => ViewportEffect::RequestHistoryWindow(query),
                    (false, None) => ViewportEffect::None,
                })
            }
            TerminalSurfaceHistoryWindowResult::HistoryChanged { .. }
            | TerminalSurfaceHistoryWindowResult::HistoryGap { .. } => {
                self.window_cache.defer_pending_request();
                self.window_cache.invalidate_rows();
                // A content-free changed/gap response is not a complete
                // replacement. Keep the last committed host frame intact;
                // the next gesture may retry or normal input may resume.
                Ok(ViewportEffect::None)
            }
        }
    }

    pub(super) fn scroll_to_offset(&mut self, offset: u64) -> ViewportEffect {
        let ViewportState::History(_) = &self.state else {
            if self.live_metrics.is_none() || offset == 0 {
                return ViewportEffect::None;
            }
            let Some(anchor) = self.window_cache.anchor() else {
                return ViewportEffect::None;
            };
            let update = self.set_history_target(offset.min(anchor.max_offset_from_bottom));
            self.state = ViewportState::History(HistoryViewport { notice: None });
            return history_window_effect(update);
        };
        if offset == 0 {
            if self.drag_grab_row.is_some() {
                return history_window_effect(self.set_history_target(0));
            }
            return self.start_resume(Vec::new());
        }
        history_window_effect(self.set_history_target(offset))
    }

    pub(super) fn scroll_metrics(&self) -> Option<TerminalScrollMetrics> {
        match &self.state {
            ViewportState::Live => self.live_metrics,
            ViewportState::History(_) => {
                if self.window_cache.visible_rows().is_some() {
                    self.window_cache
                        .anchor()
                        .map(|anchor| TerminalScrollMetrics {
                            epoch: anchor.epoch,
                            revision: anchor.revision,
                            offset_from_bottom: self.window_cache.desired_offset_from_bottom(),
                            max_offset_from_bottom: anchor.max_offset_from_bottom,
                            viewport_rows: anchor.viewport.rows,
                        })
                } else {
                    self.last_presented_scroll_metrics
                }
            }
            ViewportState::ResumePending {
                snapshot_applied,
                presented_scroll_metrics,
                ..
            } => {
                if *snapshot_applied {
                    self.live_metrics
                        .filter(|metrics| metrics.viewport_rows == self.content_size.rows)
                } else {
                    *presented_scroll_metrics
                }
            }
        }
        .filter(|metrics| metrics.is_valid())
    }

    pub(super) fn observe_presentation(&mut self) {
        if self.is_history() {
            let _ = self.window_cache.commit_visible_presentation();
        }
        self.last_presented_scroll_metrics = self.scroll_metrics();
        self.presented_gutter_column = self.gutter_column;
    }

    pub(super) fn selection_source_identity(
        &self,
        surface: &AttachmentSurface,
    ) -> Option<SelectionSourceIdentity> {
        match &self.state {
            ViewportState::Live => Some(SelectionSourceIdentity::Live {
                revision: surface.revision(),
                screen: surface.active_screen(),
                viewport: self.content_size,
            }),
            ViewportState::History(history) if history.notice.is_none() => self
                .window_cache
                .presented_slice_identity()
                .map(SelectionSourceIdentity::History),
            ViewportState::History(_) | ViewportState::ResumePending { .. } => None,
        }
    }

    pub(super) fn next_frame_selection_source_identity(
        &self,
        surface: &AttachmentSurface,
    ) -> Option<SelectionSourceIdentity> {
        match &self.state {
            ViewportState::Live => Some(SelectionSourceIdentity::Live {
                revision: surface.revision(),
                screen: surface.active_screen(),
                viewport: self.content_size,
            }),
            ViewportState::History(history) if history.notice.is_none() => self
                .window_cache
                .visible_slice_identity()
                .map(SelectionSourceIdentity::History),
            ViewportState::History(_) | ViewportState::ResumePending { .. } => None,
        }
    }

    pub(super) fn selection_rows<'a>(
        &'a self,
        surface: &'a AttachmentSurface,
    ) -> Option<&'a [TerminalSurfaceRow]> {
        match &self.state {
            ViewportState::Live => Some(&surface.surface.rows),
            ViewportState::History(history) if history.notice.is_none() => {
                self.window_cache.presented_rows()
            }
            ViewportState::History(_) | ViewportState::ResumePending { .. } => None,
        }
    }

    pub(super) fn preview_delta(
        &self,
        candidate: &AttachmentSurface,
        metrics: Option<TerminalScrollMetrics>,
        live_layout: Option<ChromeLayout>,
    ) -> ViewportDeltaPlan {
        debug_assert!(!self.is_history() || live_layout.is_none());
        let content_size = live_layout.map_or(self.content_size, |layout| layout.child);
        let gutter_column = live_layout.map_or(self.gutter_column, |layout| layout.gutter_column);
        let update_live_metrics = metrics.is_some() || self.is_live() || self.is_resume_pending();
        let live_metrics = if update_live_metrics {
            metrics
        } else {
            self.live_metrics
        };
        let anchor_observation = metrics.filter(|metrics| metrics.is_valid()).map(|metrics| {
            self.window_cache
                .preview_anchor_observation(TerminalHistoryWindowAnchor {
                    epoch: metrics.epoch,
                    revision: metrics.revision,
                    max_offset_from_bottom: metrics.max_offset_from_bottom,
                    viewport: content_size,
                })
        });
        let history_identity = match anchor_observation {
            Some(observation) => observation.presented_slice_identity(),
            None => self.window_cache.presented_slice_identity(),
        };
        let selection_source = match &self.state {
            ViewportState::Live => Some(SelectionSourceIdentity::Live {
                revision: candidate.revision(),
                screen: candidate.active_screen(),
                viewport: content_size,
            }),
            ViewportState::History(history) if history.notice.is_none() => {
                history_identity.map(SelectionSourceIdentity::History)
            }
            ViewportState::History(_) | ViewportState::ResumePending { .. } => None,
        };
        ViewportDeltaPlan {
            content_size,
            gutter_column,
            live_metrics,
            update_live_metrics,
            content_size_changed: self.content_size != content_size,
            anchor_observation,
            selection_source,
        }
    }

    pub(super) fn commit_delta(&mut self, plan: ViewportDeltaPlan) {
        if plan.update_live_metrics {
            self.live_metrics = plan.live_metrics;
        }
        if plan.content_size_changed {
            self.last_presented_scroll_metrics = None;
            if let ViewportState::ResumePending {
                presented_scroll_metrics,
                ..
            } = &mut self.state
            {
                *presented_scroll_metrics = None;
            }
        }
        self.content_size = plan.content_size;
        self.gutter_column = plan.gutter_column;
        if self.gutter_column.is_none() {
            self.drag_grab_row = None;
            self.drag_last_request = None;
            self.drag_deferred_target = None;
        }
        if let Some(observation) = plan.anchor_observation {
            let _ = self.window_cache.commit_anchor_observation(observation);
        }
    }

    pub(super) fn retain_or_resume(&mut self, bytes: Vec<u8>) -> Result<ViewportEffect, CliError> {
        match &mut self.state {
            ViewportState::Live => Ok(ViewportEffect::None),
            ViewportState::History(_) => self.begin_resume(bytes),
            ViewportState::ResumePending { retained_input, .. } => {
                append_resume_input(retained_input, &bytes)?;
                Ok(ViewportEffect::None)
            }
        }
    }

    pub(super) fn begin_resume(&mut self, bytes: Vec<u8>) -> Result<ViewportEffect, CliError> {
        match &mut self.state {
            ViewportState::ResumePending { retained_input, .. } => {
                append_resume_input(retained_input, &bytes)?;
                Ok(ViewportEffect::None)
            }
            ViewportState::Live | ViewportState::History(_) => {
                if bytes.len() > RESUME_INPUT_BOUND {
                    return Err(resume_input_overflow());
                }
                Ok(self.start_resume(bytes))
            }
        }
    }

    pub(super) fn start_resume(&mut self, retained_input: Vec<u8>) -> ViewportEffect {
        let presented_scroll_metrics = self.last_presented_scroll_metrics;
        self.discard_window_response |= self.window_cache.request_pending();
        self.window_cache.invalidate();
        self.drag_grab_row = None;
        self.drag_last_request = None;
        self.drag_deferred_target = None;
        self.state = ViewportState::ResumePending {
            retained_input,
            snapshot_applied: false,
            presented_scroll_metrics,
        };
        ViewportEffect::Resume
    }

    pub(super) fn retain_resume_input(&mut self, bytes: &[u8]) -> Result<(), CliError> {
        let ViewportState::ResumePending { retained_input, .. } = &mut self.state else {
            return Ok(());
        };
        append_resume_input(retained_input, bytes)
    }

    pub(super) fn observe_snapshot(&mut self, metrics: Option<TerminalScrollMetrics>) {
        self.live_metrics = metrics;
        self.observe_window_anchor(metrics);
        match &mut self.state {
            ViewportState::Live | ViewportState::History(_) => {}
            ViewportState::ResumePending {
                snapshot_applied, ..
            } => *snapshot_applied = true,
        }
    }

    pub(super) fn observe_sync_required(&mut self) {
        if self.is_live() {
            let _ = self.start_resume(Vec::new());
        }
    }

    pub(super) fn reset_presentation_for_reconnect(&mut self) {
        self.drag_grab_row = None;
        self.drag_last_request = None;
        self.drag_deferred_target = None;
        if self.is_history() {
            let _ = self.start_resume(Vec::new());
        } else {
            self.discard_window_response |= self.window_cache.request_pending();
            self.window_cache.invalidate();
        }
        // A replacement attachment epoch cannot authenticate even the
        // previously live extent. Keep the old pixels in place, but do
        // not project their metrics into reconnecting chrome.
        self.live_metrics = None;
        self.last_presented_scroll_metrics = None;
        if let ViewportState::ResumePending {
            snapshot_applied,
            presented_scroll_metrics,
            ..
        } = &mut self.state
        {
            // A transport reconnect has no validated relationship to the
            // pixels from the previous attachment epoch.
            *snapshot_applied = false;
            *presented_scroll_metrics = None;
        }
    }

    pub(super) fn finish_resume(&mut self) -> Option<Vec<u8>> {
        let ViewportState::ResumePending {
            snapshot_applied: true,
            ..
        } = &self.state
        else {
            return None;
        };
        let ViewportState::ResumePending { retained_input, .. } =
            std::mem::replace(&mut self.state, ViewportState::Live)
        else {
            unreachable!();
        };
        Some(retained_input)
    }

    pub(super) fn visible_semantic_history_rows(
        &self,
    ) -> Option<(Vec<&TerminalSurfaceRow>, usize, Option<&'static str>)> {
        let ViewportState::History(history) = &self.state else {
            return None;
        };
        let rows = self.window_cache.visible_rows()?;
        let rows = rows.iter().collect();
        Some((rows, usize::from(self.content_size.rows), history.notice))
    }
}

fn history_window_effect(update: ViewportCacheUpdate) -> ViewportEffect {
    match (update.render_local, update.request) {
        (true, Some(query)) => ViewportEffect::RenderAndRequestHistoryWindow(query),
        (true, None) => ViewportEffect::Render,
        (false, Some(query)) => ViewportEffect::RequestHistoryWindow(query),
        (false, None) => ViewportEffect::None,
    }
}
