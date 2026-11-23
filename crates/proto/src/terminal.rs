//! Wire conversions for stable identifiers and semantic terminal state.

use voytty_core::terminal::{
    ActiveScreen, TerminalAppearance, TerminalClipboardWrite, TerminalColor, TerminalColorProfile,
    TerminalColorSnapshot, TerminalColorValue, TerminalCursor, TerminalHistoryWindowAnchor,
    TerminalHistoryWindowQuery, TerminalKeyboardFlags, TerminalModes, TerminalMouseEncoding,
    TerminalMouseMode, TerminalNotification, TerminalScrollMetrics, TerminalSize, TerminalStyle,
    TerminalSurface, TerminalSurfaceDelta, TerminalSurfaceError, TerminalSurfaceHistoryWindowFrame,
    TerminalSurfaceHistoryWindowResult, TerminalSurfaceRowPatch, TerminalSurfaceSnapshot,
    TerminalUnderline, TerminalViewportDisposition,
};
use voytty_core::{
    AttachmentId, DaemonIncarnation, DeviceId, OperationId, OperationLease, ResumeViewId, SessionId,
};

use crate::terminal_links::{LinkEncoder, decode_links, decode_row};
use crate::{ProtocolError, v2};

impl From<DeviceId> for v2::DeviceId {
    fn from(value: DeviceId) -> Self {
        Self {
            value: value.to_bytes().to_vec(),
        }
    }
}

impl TryFrom<v2::DeviceId> for DeviceId {
    type Error = ProtocolError;

    fn try_from(value: v2::DeviceId) -> Result<Self, Self::Error> {
        Self::from_bytes(&value.value).map_err(ProtocolError::InvalidIdentifier)
    }
}

impl From<SessionId> for v2::SessionId {
    fn from(value: SessionId) -> Self {
        Self {
            value: value.to_bytes().to_vec(),
        }
    }
}

impl TryFrom<v2::SessionId> for SessionId {
    type Error = ProtocolError;

    fn try_from(value: v2::SessionId) -> Result<Self, Self::Error> {
        Self::from_bytes(&value.value).map_err(ProtocolError::InvalidIdentifier)
    }
}

impl From<AttachmentId> for v2::AttachmentId {
    fn from(value: AttachmentId) -> Self {
        Self {
            value: value.to_bytes().to_vec(),
        }
    }
}

impl TryFrom<v2::AttachmentId> for AttachmentId {
    type Error = ProtocolError;

    fn try_from(value: v2::AttachmentId) -> Result<Self, Self::Error> {
        Self::from_bytes(&value.value).map_err(ProtocolError::InvalidIdentifier)
    }
}

impl From<ResumeViewId> for v2::ResumeViewId {
    fn from(value: ResumeViewId) -> Self {
        Self {
            value: value.to_bytes().to_vec(),
        }
    }
}

impl TryFrom<v2::ResumeViewId> for ResumeViewId {
    type Error = ProtocolError;

    fn try_from(value: v2::ResumeViewId) -> Result<Self, Self::Error> {
        Self::from_bytes(&value.value).map_err(ProtocolError::InvalidIdentifier)
    }
}

impl From<OperationId> for v2::OperationId {
    fn from(value: OperationId) -> Self {
        Self {
            lease_ordinal: value.lease.ordinal,
            sequence: value.sequence,
            daemon_incarnation: value.lease.daemon_incarnation.to_bytes().to_vec(),
        }
    }
}

impl TryFrom<v2::OperationId> for OperationId {
    type Error = ProtocolError;

    fn try_from(value: v2::OperationId) -> Result<Self, Self::Error> {
        Ok(Self {
            lease: OperationLease {
                daemon_incarnation: DaemonIncarnation::from_bytes(&value.daemon_incarnation)
                    .map_err(ProtocolError::InvalidIdentifier)?,
                ordinal: value.lease_ordinal,
            },
            sequence: value.sequence,
        })
    }
}

impl From<OperationLease> for v2::OperationLease {
    fn from(value: OperationLease) -> Self {
        Self {
            daemon_incarnation: value.daemon_incarnation.to_bytes().to_vec(),
            ordinal: value.ordinal,
        }
    }
}

impl TryFrom<v2::OperationLease> for OperationLease {
    type Error = ProtocolError;

    fn try_from(value: v2::OperationLease) -> Result<Self, Self::Error> {
        Ok(Self {
            daemon_incarnation: DaemonIncarnation::from_bytes(&value.daemon_incarnation)
                .map_err(ProtocolError::InvalidIdentifier)?,
            ordinal: value.ordinal,
        })
    }
}

impl From<TerminalSize> for v2::TerminalViewport {
    fn from(value: TerminalSize) -> Self {
        Self {
            rows: u32::from(value.rows),
            columns: u32::from(value.columns),
        }
    }
}

impl TryFrom<v2::TerminalViewport> for TerminalSize {
    type Error = ProtocolError;

    fn try_from(value: v2::TerminalViewport) -> Result<Self, Self::Error> {
        let rows = u16::try_from(value.rows).map_err(|_| ProtocolError::InvalidTerminalSize {
            rows: value.rows,
            columns: value.columns,
        })?;
        let columns =
            u16::try_from(value.columns).map_err(|_| ProtocolError::InvalidTerminalSize {
                rows: value.rows,
                columns: value.columns,
            })?;
        if rows == 0 || columns == 0 {
            return Err(ProtocolError::InvalidTerminalSize {
                rows: value.rows,
                columns: value.columns,
            });
        }
        Ok(TerminalSize::new(rows, columns))
    }
}

impl From<ActiveScreen> for v2::TerminalActiveScreen {
    fn from(value: ActiveScreen) -> Self {
        match value {
            ActiveScreen::Main => Self::Main,
            ActiveScreen::Alternate => Self::Alternate,
        }
    }
}

impl From<TerminalModes> for v2::TerminalModes {
    fn from(value: TerminalModes) -> Self {
        Self {
            application_keypad: value.application_keypad,
            application_cursor: value.application_cursor,
            bracketed_paste: value.bracketed_paste,
            focus_reporting: value.focus_reporting,
            mouse_mode: match value.mouse_mode {
                TerminalMouseMode::None => 0,
                TerminalMouseMode::Press => 1,
                TerminalMouseMode::PressRelease => 2,
                TerminalMouseMode::ButtonMotion => 3,
                TerminalMouseMode::AnyMotion => 4,
            },
            mouse_encoding: match value.mouse_encoding {
                TerminalMouseEncoding::Default => 0,
                TerminalMouseEncoding::Utf8 => 1,
                TerminalMouseEncoding::Sgr => 2,
            },
            alternate_scroll: value.alternate_scroll,
            keyboard_flags: u32::from(value.keyboard_flags.bits()),
        }
    }
}

impl TryFrom<v2::TerminalModes> for TerminalModes {
    type Error = ProtocolError;

    fn try_from(value: v2::TerminalModes) -> Result<Self, Self::Error> {
        let mouse_mode = match value.mouse_mode {
            0 => TerminalMouseMode::None,
            1 => TerminalMouseMode::Press,
            2 => TerminalMouseMode::PressRelease,
            3 => TerminalMouseMode::ButtonMotion,
            4 => TerminalMouseMode::AnyMotion,
            _ => return Err(ProtocolError::InvalidTerminalSemanticField("mouse_mode")),
        };
        let mouse_encoding = match value.mouse_encoding {
            0 => TerminalMouseEncoding::Default,
            1 => TerminalMouseEncoding::Utf8,
            2 => TerminalMouseEncoding::Sgr,
            _ => {
                return Err(ProtocolError::InvalidTerminalSemanticField(
                    "mouse_encoding",
                ));
            }
        };
        let keyboard_flags = u8::try_from(value.keyboard_flags)
            .ok()
            .and_then(TerminalKeyboardFlags::from_bits)
            .ok_or(ProtocolError::InvalidTerminalSemanticField(
                "keyboard_flags",
            ))?;
        Ok(Self {
            application_keypad: value.application_keypad,
            application_cursor: value.application_cursor,
            bracketed_paste: value.bracketed_paste,
            focus_reporting: value.focus_reporting,
            alternate_scroll: value.alternate_scroll,
            mouse_mode,
            mouse_encoding,
            keyboard_flags,
        })
    }
}

/// Projects a validated notification into its attachment-scoped wire message.
#[must_use]
pub fn terminal_notification_message(
    attachment_id: AttachmentId,
    notification: TerminalNotification,
) -> v2::TerminalNotification {
    use v2::terminal_notification::Content;
    let content = match notification.title() {
        None => Content::Osc9(notification.body().to_owned()),
        Some(title) => Content::Osc777(v2::TerminalNotificationOsc777 {
            title: title.to_owned(),
            body: notification.body().to_owned(),
        }),
    };
    v2::TerminalNotification {
        attachment_id: Some(attachment_id.into()),
        content: Some(content),
    }
}

/// Validates notification form, target identity, text and canonical encoded size.
pub fn terminal_notification_from_message(
    value: v2::TerminalNotification,
) -> Result<(AttachmentId, TerminalNotification), ProtocolError> {
    use v2::terminal_notification::Content;
    let invalid = || ProtocolError::InvalidTerminalSemanticField("notification");
    let attachment_id = value.attachment_id.ok_or_else(invalid)?.try_into()?;
    let notification = match value.content.ok_or_else(invalid)? {
        Content::Osc9(message) => TerminalNotification::osc9(message),
        Content::Osc777(value) => TerminalNotification::osc777(value.title, value.body),
    }
    .map_err(|_| invalid())?;
    Ok((attachment_id, notification))
}

/// Projects a validated decoded clipboard write into its redacted wire DTO.
#[must_use]
pub fn terminal_clipboard_write_message(
    attachment_id: AttachmentId,
    write: TerminalClipboardWrite,
) -> v2::TerminalClipboardWrite {
    v2::TerminalClipboardWrite {
        attachment_id: Some(attachment_id.into()),
        text: write.into_string(),
    }
}

/// Validates a decoded clipboard write at the protocol boundary.
pub fn terminal_clipboard_write_from_message(
    value: v2::TerminalClipboardWrite,
) -> Result<(AttachmentId, TerminalClipboardWrite), ProtocolError> {
    let attachment_id = value
        .attachment_id
        .ok_or(ProtocolError::InvalidTerminalSemanticField("attachment_id"))?
        .try_into()?;
    let write = TerminalClipboardWrite::new(value.text)
        .map_err(|_| ProtocolError::InvalidTerminalSemanticField("clipboard_text"))?;
    Ok((attachment_id, write))
}

impl From<TerminalColorProfile> for v2::TerminalColorProfile {
    fn from(profile: TerminalColorProfile) -> Self {
        use v2::terminal_color_observation::Value;
        Self {
            appearance: profile.appearance as u32,
            values: profile
                .values
                .into_iter()
                .map(|value| v2::TerminalColorObservation {
                    value: Some(match value {
                        TerminalColorValue::Unknown => Value::Unknown(true),
                        TerminalColorValue::Dynamic => Value::Dynamic(true),
                        TerminalColorValue::Rgb(r, g, b) => Value::Rgb(v2::TerminalRgbColor {
                            red: r.into(),
                            green: g.into(),
                            blue: b.into(),
                        }),
                    }),
                })
                .collect(),
        }
    }
}
impl TryFrom<v2::TerminalColorProfile> for TerminalColorProfile {
    type Error = ProtocolError;
    fn try_from(profile: v2::TerminalColorProfile) -> Result<Self, Self::Error> {
        use v2::terminal_color_observation::Value;
        let invalid = || ProtocolError::InvalidTerminalSemanticField("color_profile");
        if profile.values.len() != voytty_core::terminal::TERMINAL_COLOR_SLOTS {
            return Err(invalid());
        }
        let mut result = Self {
            appearance: match profile.appearance {
                0 => TerminalAppearance::Unknown,
                1 => TerminalAppearance::Dark,
                2 => TerminalAppearance::Light,
                _ => return Err(invalid()),
            },
            ..Self::default()
        };
        for (slot, value) in profile.values.into_iter().enumerate() {
            result.values[slot] = match value.value {
                Some(Value::Unknown(true)) => TerminalColorValue::Unknown,
                Some(Value::Dynamic(true)) => TerminalColorValue::Dynamic,
                Some(Value::Rgb(c)) => TerminalColorValue::Rgb(
                    c.red.try_into().map_err(|_| invalid())?,
                    c.green.try_into().map_err(|_| invalid())?,
                    c.blue.try_into().map_err(|_| invalid())?,
                ),
                _ => return Err(invalid()),
            };
        }
        if !result.is_valid() {
            return Err(invalid());
        }
        Ok(result)
    }
}
impl From<TerminalColorSnapshot> for v2::TerminalColorSnapshot {
    fn from(c: TerminalColorSnapshot) -> Self {
        Self {
            profile: Some(c.profile.into()),
            changed_at: c.changed_at.get(),
            reverse: c.reverse,
            custom_cursor: c.custom_cursor,
            inherited_sources: c.inherited_sources.into_iter().map(u32::from).collect(),
        }
    }
}
pub(crate) fn required_colors(
    c: Option<v2::TerminalColorSnapshot>,
) -> Result<TerminalColorSnapshot, ProtocolError> {
    let invalid = || ProtocolError::InvalidTerminalSemanticField("colors");
    let c = c.ok_or_else(invalid)?;
    if c.inherited_sources.len() != voytty_core::terminal::TERMINAL_COLOR_SLOTS {
        return Err(invalid());
    }
    Ok(TerminalColorSnapshot {
        profile: c.profile.ok_or_else(invalid)?.try_into()?,
        changed_at: voytty_core::Revision::new(c.changed_at),
        reverse: c.reverse,
        custom_cursor: c.custom_cursor,
        inherited_sources: c
            .inherited_sources
            .into_iter()
            .map(|source| u16::try_from(source).map_err(|_| invalid()))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| invalid())?,
    })
}

impl From<TerminalColor> for v2::TerminalColor {
    fn from(value: TerminalColor) -> Self {
        use v2::terminal_color::Value;
        let value = match value {
            TerminalColor::Default => Value::DefaultColor(true),
            TerminalColor::Indexed(index) => Value::Indexed(u32::from(index)),
            TerminalColor::Rgb(red, green, blue) => Value::Rgb(v2::TerminalRgbColor {
                red: u32::from(red),
                green: u32::from(green),
                blue: u32::from(blue),
            }),
        };
        Self { value: Some(value) }
    }
}

impl TryFrom<v2::TerminalColor> for TerminalColor {
    type Error = ProtocolError;

    fn try_from(value: v2::TerminalColor) -> Result<Self, Self::Error> {
        use v2::terminal_color::Value;
        match value.value {
            Some(Value::DefaultColor(true)) => Ok(Self::Default),
            Some(Value::DefaultColor(false)) | None => Err(
                ProtocolError::InvalidTerminalSemanticField("terminal_color"),
            ),
            Some(Value::Indexed(index)) => u8::try_from(index)
                .map(Self::Indexed)
                .map_err(|_| ProtocolError::InvalidTerminalSemanticField("indexed_color")),
            Some(Value::Rgb(rgb)) => Ok(Self::Rgb(
                u8::try_from(rgb.red)
                    .map_err(|_| ProtocolError::InvalidTerminalSemanticField("rgb_red"))?,
                u8::try_from(rgb.green)
                    .map_err(|_| ProtocolError::InvalidTerminalSemanticField("rgb_green"))?,
                u8::try_from(rgb.blue)
                    .map_err(|_| ProtocolError::InvalidTerminalSemanticField("rgb_blue"))?,
            )),
        }
    }
}

impl From<TerminalStyle> for v2::TerminalStyle {
    fn from(value: TerminalStyle) -> Self {
        Self {
            foreground: Some(value.foreground.into()),
            background: Some(value.background.into()),
            bold: value.bold,
            dim: value.dim,
            italic: value.italic,
            underline: value.underline as u32,
            underline_color: Some(value.underline_color.into()),
            inverse: value.inverse,
            strike: value.strike,
            conceal: value.conceal,
        }
    }
}

impl TryFrom<v2::TerminalStyle> for TerminalStyle {
    type Error = ProtocolError;

    fn try_from(value: v2::TerminalStyle) -> Result<Self, Self::Error> {
        Ok(Self {
            foreground: value
                .foreground
                .ok_or(ProtocolError::InvalidTerminalSemanticField("foreground"))?
                .try_into()?,
            background: value
                .background
                .ok_or(ProtocolError::InvalidTerminalSemanticField("background"))?
                .try_into()?,
            bold: value.bold,
            dim: value.dim,
            italic: value.italic,
            underline: match value.underline {
                0 => TerminalUnderline::None,
                1 => TerminalUnderline::Single,
                2 => TerminalUnderline::Double,
                3 => TerminalUnderline::Curly,
                4 => TerminalUnderline::Dotted,
                5 => TerminalUnderline::Dashed,
                _ => return Err(ProtocolError::InvalidTerminalSemanticField("underline")),
            },
            underline_color: value
                .underline_color
                .ok_or(ProtocolError::InvalidTerminalSemanticField(
                    "underline_color",
                ))?
                .try_into()?,
            inverse: value.inverse,
            strike: value.strike,
            conceal: value.conceal,
        })
    }
}

impl From<TerminalCursor> for v2::TerminalCursor {
    fn from(value: TerminalCursor) -> Self {
        Self {
            presentation: Some(v2::TerminalCursorPresentation {
                shape: match value.presentation.shape {
                    voytty_core::terminal::TerminalCursorShape::Block => 1,
                    voytty_core::terminal::TerminalCursorShape::Beam => 2,
                    voytty_core::terminal::TerminalCursorShape::Underline => 3,
                },
                blinking: value.presentation.blinking,
            }),
            row: u32::from(value.row),
            column: u32::from(value.column),
            visible: value.visible,
            style: Some(value.style.into()),
        }
    }
}

impl TryFrom<v2::TerminalCursor> for TerminalCursor {
    type Error = ProtocolError;

    fn try_from(value: v2::TerminalCursor) -> Result<Self, Self::Error> {
        let presentation =
            value
                .presentation
                .ok_or(ProtocolError::InvalidTerminalSemanticField(
                    "cursor_presentation",
                ))?;
        Ok(Self {
            presentation: voytty_core::terminal::TerminalCursorPresentation {
                shape: match presentation.shape {
                    1 => voytty_core::terminal::TerminalCursorShape::Block,
                    2 => voytty_core::terminal::TerminalCursorShape::Beam,
                    3 => voytty_core::terminal::TerminalCursorShape::Underline,
                    _ => return Err(ProtocolError::InvalidTerminalSemanticField("cursor_shape")),
                },
                blinking: presentation.blinking,
            },
            row: u16::try_from(value.row)
                .map_err(|_| ProtocolError::InvalidTerminalSemanticField("cursor_row"))?,
            column: u16::try_from(value.column)
                .map_err(|_| ProtocolError::InvalidTerminalSemanticField("cursor_column"))?,
            visible: value.visible,
            style: value
                .style
                .ok_or(ProtocolError::InvalidTerminalSemanticField("cursor_style"))?
                .try_into()?,
        })
    }
}

impl From<TerminalScrollMetrics> for v2::TerminalScrollMetrics {
    fn from(value: TerminalScrollMetrics) -> Self {
        Self {
            epoch: value.epoch.get(),
            revision: value.revision.get(),
            offset_from_bottom: value.offset_from_bottom,
            max_offset_from_bottom: value.max_offset_from_bottom,
            viewport_rows: u32::from(value.viewport_rows),
        }
    }
}

impl From<TerminalHistoryWindowAnchor> for v2::TerminalHistoryWindowAnchor {
    fn from(value: TerminalHistoryWindowAnchor) -> Self {
        Self {
            epoch: value.epoch.get(),
            revision: value.revision.get(),
            max_offset_from_bottom: value.max_offset_from_bottom,
            viewport_rows: u32::from(value.viewport.rows),
            viewport_columns: u32::from(value.viewport.columns),
        }
    }
}

fn terminal_active_screen(value: i32) -> Result<ActiveScreen, ProtocolError> {
    match v2::TerminalActiveScreen::try_from(value).ok() {
        Some(v2::TerminalActiveScreen::Main) => Ok(ActiveScreen::Main),
        Some(v2::TerminalActiveScreen::Alternate) => Ok(ActiveScreen::Alternate),
        Some(v2::TerminalActiveScreen::Unspecified) | None => {
            Err(ProtocolError::InvalidTerminalSemanticField("active_screen"))
        }
    }
}

fn terminal_scroll_metrics(
    value: v2::TerminalScrollMetrics,
) -> Result<TerminalScrollMetrics, ProtocolError> {
    Ok(TerminalScrollMetrics {
        epoch: voytty_core::Revision::new(value.epoch),
        revision: voytty_core::Revision::new(value.revision),
        offset_from_bottom: value.offset_from_bottom,
        max_offset_from_bottom: value.max_offset_from_bottom,
        viewport_rows: u16::try_from(value.viewport_rows).map_err(|_| {
            ProtocolError::InvalidTerminalSemanticField("scroll_metrics_viewport_rows")
        })?,
    })
}

fn terminal_surface_message(surface: TerminalSurface) -> v2::TerminalSurface {
    let mut links = LinkEncoder::default();
    v2::TerminalSurface {
        application_title: surface.application_title,
        colors: Some(surface.colors.into()),
        row_count: u32::from(surface.size.rows),
        column_count: u32::from(surface.size.columns),
        active_screen: v2::TerminalActiveScreen::from(surface.active_screen) as i32,
        rows: surface.rows.into_iter().map(|row| links.row(row)).collect(),
        cursor: Some(surface.cursor.into()),
        modes: Some(surface.modes.into()),
        scroll_metrics: surface.scroll_metrics.map(Into::into),
        hyperlinks: links.finish(),
    }
}

fn terminal_surface_from_message(
    value: v2::TerminalSurface,
    revision: voytty_core::Revision,
) -> Result<TerminalSurface, ProtocolError> {
    let links = decode_links(value.hyperlinks)?;
    let size = TerminalSize::try_from(v2::TerminalViewport {
        rows: value.row_count,
        columns: value.column_count,
    })?;
    let surface = TerminalSurface {
        application_title: value.application_title,
        colors: required_colors(value.colors)?,
        size,
        active_screen: terminal_active_screen(value.active_screen)?,
        rows: value
            .rows
            .into_iter()
            .map(|row| decode_row(row, &links))
            .collect::<Result<_, _>>()?,
        cursor: value
            .cursor
            .ok_or(ProtocolError::InvalidTerminalSemanticField("cursor"))?
            .try_into()?,
        modes: value
            .modes
            .ok_or(ProtocolError::InvalidTerminalSemanticField("modes"))?
            .try_into()?,
        scroll_metrics: value
            .scroll_metrics
            .map(terminal_scroll_metrics)
            .transpose()?,
    };
    surface
        .validate(revision)
        .map_err(ProtocolError::InvalidTerminalSurface)?;
    Ok(surface)
}

/// Projects one exact semantic snapshot into its content-redacted wire message.
#[must_use]
pub fn terminal_surface_snapshot_message(
    session_id: SessionId,
    attachment_id: AttachmentId,
    snapshot: TerminalSurfaceSnapshot,
) -> v2::TerminalSemanticSnapshot {
    v2::TerminalSemanticSnapshot {
        session_id: Some(session_id.into()),
        attachment_id: Some(attachment_id.into()),
        revision: snapshot.revision.get(),
        surface: Some(terminal_surface_message(snapshot.surface)),
        // Numbered only by a host stream owner after explicit opt-in.
        frame_sequence: 0,
    }
}

/// Validates and converts one exact semantic snapshot wire message.
pub fn terminal_surface_snapshot_from_message(
    value: v2::TerminalSemanticSnapshot,
) -> Result<(SessionId, AttachmentId, TerminalSurfaceSnapshot), ProtocolError> {
    let session_id = value
        .session_id
        .ok_or(ProtocolError::InvalidTerminalSemanticField("session_id"))?
        .try_into()?;
    let attachment_id = value
        .attachment_id
        .ok_or(ProtocolError::InvalidTerminalSemanticField("attachment_id"))?
        .try_into()?;
    let revision = voytty_core::Revision::new(value.revision);
    let surface = terminal_surface_from_message(
        value
            .surface
            .ok_or(ProtocolError::InvalidTerminalSemanticField("surface"))?,
        revision,
    )?;
    Ok((
        session_id,
        attachment_id,
        TerminalSurfaceSnapshot { revision, surface },
    ))
}

/// Projects one exact semantic delta into its content-redacted wire message.
#[must_use]
pub fn terminal_surface_delta_message(
    attachment_id: AttachmentId,
    delta: TerminalSurfaceDelta,
) -> v2::TerminalSemanticDelta {
    let mut links = LinkEncoder::default();
    v2::TerminalSemanticDelta {
        application_title: delta.application_title,
        colors: Some(delta.colors.into()),
        attachment_id: Some(attachment_id.into()),
        from_revision: delta.from_revision.get(),
        to_revision: delta.to_revision.get(),
        row_count: u32::from(delta.size.rows),
        column_count: u32::from(delta.size.columns),
        active_screen: v2::TerminalActiveScreen::from(delta.active_screen) as i32,
        row_patches: delta
            .row_patches
            .into_iter()
            .map(|patch| v2::TerminalSemanticRowPatch {
                row: u32::from(patch.row),
                replacement: Some(links.row(patch.replacement)),
            })
            .collect(),
        cursor: Some(delta.cursor.into()),
        modes: Some(delta.modes.into()),
        scroll_metrics: delta.scroll_metrics.map(Into::into),
        hyperlinks: links.finish(),
        // Numbered only by a host stream owner after explicit opt-in.
        frame_sequence: 0,
    }
}

/// Validates and converts one exact semantic delta wire message.
pub fn terminal_surface_delta_from_message(
    value: v2::TerminalSemanticDelta,
) -> Result<(AttachmentId, TerminalSurfaceDelta), ProtocolError> {
    let links = decode_links(value.hyperlinks)?;
    let attachment_id = value
        .attachment_id
        .ok_or(ProtocolError::InvalidTerminalSemanticField("attachment_id"))?
        .try_into()?;
    let delta = TerminalSurfaceDelta {
        application_title: value.application_title,
        colors: required_colors(value.colors)?,
        from_revision: voytty_core::Revision::new(value.from_revision),
        to_revision: voytty_core::Revision::new(value.to_revision),
        size: TerminalSize::try_from(v2::TerminalViewport {
            rows: value.row_count,
            columns: value.column_count,
        })?,
        active_screen: terminal_active_screen(value.active_screen)?,
        row_patches: value
            .row_patches
            .into_iter()
            .map(|patch| {
                Ok(TerminalSurfaceRowPatch {
                    row: u16::try_from(patch.row).map_err(|_| {
                        ProtocolError::InvalidTerminalSemanticField("row_patch_index")
                    })?,
                    replacement: decode_row(
                        patch
                            .replacement
                            .ok_or(ProtocolError::InvalidTerminalSemanticField(
                                "row_patch_replacement",
                            ))?,
                        &links,
                    )?,
                })
            })
            .collect::<Result<_, ProtocolError>>()?,
        cursor: value
            .cursor
            .ok_or(ProtocolError::InvalidTerminalSemanticField("cursor"))?
            .try_into()?,
        modes: value
            .modes
            .ok_or(ProtocolError::InvalidTerminalSemanticField("modes"))?
            .try_into()?,
        scroll_metrics: value
            .scroll_metrics
            .map(terminal_scroll_metrics)
            .transpose()?,
    };
    delta
        .validate()
        .map_err(ProtocolError::InvalidTerminalSurface)?;
    Ok((attachment_id, delta))
}

/// Projects one semantic history-window result into a content-redacted wire message.
#[must_use]
pub fn terminal_surface_history_window_frame_message(
    attachment_id: AttachmentId,
    result: TerminalSurfaceHistoryWindowResult,
) -> v2::TerminalSemanticHistoryWindowFrame {
    let mut links = LinkEncoder::default();
    match result {
        TerminalSurfaceHistoryWindowResult::Frame(TerminalSurfaceHistoryWindowFrame {
            colors,
            disposition,
            anchor,
            target_offset_from_bottom,
            first_row_from_live_top,
            rows,
        }) => v2::TerminalSemanticHistoryWindowFrame {
            colors: Some(colors.into()),
            attachment_id: Some(attachment_id.into()),
            outcome: v2::TerminalHistoryWindowOutcome::Frame as i32,
            disposition: match disposition {
                TerminalViewportDisposition::Exact => v2::TerminalViewportDisposition::Exact as i32,
                TerminalViewportDisposition::Rebased => {
                    v2::TerminalViewportDisposition::Rebased as i32
                }
            },
            anchor: Some(anchor.into()),
            target_offset_from_bottom,
            first_row_from_live_top,
            rows: rows.into_iter().map(|row| links.row(row)).collect(),
            hyperlinks: links.finish(),
            current_epoch: anchor.epoch.get(),
            current_revision: anchor.revision.get(),
        },
        TerminalSurfaceHistoryWindowResult::HistoryChanged { epoch, revision } => {
            v2::TerminalSemanticHistoryWindowFrame {
                colors: None,
                attachment_id: Some(attachment_id.into()),
                outcome: v2::TerminalHistoryWindowOutcome::Changed as i32,
                disposition: v2::TerminalViewportDisposition::Unspecified as i32,
                anchor: None,
                target_offset_from_bottom: 0,
                first_row_from_live_top: 0,
                rows: Vec::new(),
                hyperlinks: Vec::new(),
                current_epoch: epoch.get(),
                current_revision: revision.get(),
            }
        }
        TerminalSurfaceHistoryWindowResult::HistoryGap { epoch, revision } => {
            v2::TerminalSemanticHistoryWindowFrame {
                colors: None,
                attachment_id: Some(attachment_id.into()),
                outcome: v2::TerminalHistoryWindowOutcome::Gap as i32,
                disposition: v2::TerminalViewportDisposition::Unspecified as i32,
                anchor: None,
                target_offset_from_bottom: 0,
                first_row_from_live_top: 0,
                rows: Vec::new(),
                hyperlinks: Vec::new(),
                current_epoch: epoch.get(),
                current_revision: revision.get(),
            }
        }
    }
}

/// Validates and converts one request-bound semantic history-window response.
pub fn terminal_surface_history_window_from_message(
    value: v2::TerminalSemanticHistoryWindowFrame,
    query: TerminalHistoryWindowQuery,
) -> Result<(AttachmentId, TerminalSurfaceHistoryWindowResult), ProtocolError> {
    let links = decode_links(value.hyperlinks)?;
    let attachment_id = value
        .attachment_id
        .ok_or(ProtocolError::InvalidTerminalSemanticField("attachment_id"))?
        .try_into()?;
    let outcome = v2::TerminalHistoryWindowOutcome::try_from(value.outcome)
        .map_err(|_| ProtocolError::InvalidTerminalSemanticField("history_window_outcome"))?;
    let result = match outcome {
        v2::TerminalHistoryWindowOutcome::Frame => {
            let disposition = match v2::TerminalViewportDisposition::try_from(value.disposition)
                .ok()
            {
                Some(v2::TerminalViewportDisposition::Exact) => TerminalViewportDisposition::Exact,
                Some(v2::TerminalViewportDisposition::Rebased) => {
                    TerminalViewportDisposition::Rebased
                }
                _ => {
                    return Err(ProtocolError::InvalidTerminalSemanticField(
                        "history_window_disposition",
                    ));
                }
            };
            let anchor = value
                .anchor
                .ok_or(ProtocolError::InvalidTerminalSemanticField(
                    "history_window_anchor",
                ))?;
            let anchor = TerminalHistoryWindowAnchor {
                epoch: voytty_core::Revision::new(anchor.epoch),
                revision: voytty_core::Revision::new(anchor.revision),
                max_offset_from_bottom: anchor.max_offset_from_bottom,
                viewport: TerminalSize::try_from(v2::TerminalViewport {
                    rows: anchor.viewport_rows,
                    columns: anchor.viewport_columns,
                })?,
            };
            if value.current_epoch != anchor.epoch.get()
                || value.current_revision != anchor.revision.get()
            {
                return Err(ProtocolError::InvalidTerminalSemanticField(
                    "history_window_current_revision",
                ));
            }
            let frame = TerminalSurfaceHistoryWindowFrame {
                colors: required_colors(value.colors)?,
                disposition,
                anchor,
                target_offset_from_bottom: value.target_offset_from_bottom,
                first_row_from_live_top: value.first_row_from_live_top,
                rows: value
                    .rows
                    .into_iter()
                    .map(|row| decode_row(row, &links))
                    .collect::<Result<_, _>>()?,
            };
            frame
                .validate_for(query)
                .map_err(ProtocolError::InvalidTerminalSurface)?;
            TerminalSurfaceHistoryWindowResult::Frame(frame)
        }
        v2::TerminalHistoryWindowOutcome::Changed | v2::TerminalHistoryWindowOutcome::Gap => {
            if value.disposition != v2::TerminalViewportDisposition::Unspecified as i32
                || value.colors.is_some()
                || value.anchor.is_some()
                || value.target_offset_from_bottom != 0
                || value.first_row_from_live_top != 0
                || !value.rows.is_empty()
                || !links.is_empty()
                || value.current_epoch > value.current_revision
                || value.current_revision < query.anchor.revision.get()
            {
                return Err(ProtocolError::InvalidTerminalSurface(
                    TerminalSurfaceError::InvalidHistoryWindow,
                ));
            }
            let fields = {
                let epoch = voytty_core::Revision::new(value.current_epoch);
                let revision = voytty_core::Revision::new(value.current_revision);
                (epoch, revision)
            };
            if outcome == v2::TerminalHistoryWindowOutcome::Changed {
                TerminalSurfaceHistoryWindowResult::HistoryChanged {
                    epoch: fields.0,
                    revision: fields.1,
                }
            } else {
                TerminalSurfaceHistoryWindowResult::HistoryGap {
                    epoch: fields.0,
                    revision: fields.1,
                }
            }
        }
        v2::TerminalHistoryWindowOutcome::Unspecified => {
            return Err(ProtocolError::InvalidTerminalSemanticField(
                "history_window_outcome",
            ));
        }
    };
    Ok((attachment_id, result))
}
