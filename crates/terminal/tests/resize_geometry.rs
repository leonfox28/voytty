//! Resize presentation without a child repaint or a time-based hold.

use zterm_core::terminal::{ActiveScreen, TerminalSize, TerminalSurfaceDeltaResult};
use zterm_terminal::TerminalModel;

#[test]
fn main_resize_keeps_the_prompt_while_alternate_resize_keeps_top_rows() {
    for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
        for cursor in [0_u16, 2, 5] {
            for visible in [false, true] {
                let mut model = TerminalModel::new(TerminalSize::new(6, 16), 32).expect("model");
                if screen == ActiveScreen::Alternate {
                    model.ingest(b"\x1b[?1049h").expect("alternate");
                }
                for row in 1..=6 {
                    model
                        .ingest(format!("\x1b[{row};1H\x1b[3{row}mROW {row}\x1b[0m").as_bytes())
                        .expect("distinct styled rows");
                }
                model
                    .ingest(
                        format!(
                            "\x1b[{};4H\x1b[?25{}",
                            cursor + 1,
                            if visible { 'h' } else { 'l' }
                        )
                        .as_bytes(),
                    )
                    .expect("cursor");
                let before = model.snapshot();
                let resized = model.resize(TerminalSize::new(3, 16)).expect("shrink");
                // A quiet child must not need output or a timeout to publish.
                assert_eq!(model.published_revision(), resized.revision);
                assert_eq!(model.presentation_deadline(), None);
                let small = model.snapshot();
                small.validate().expect("clipped snapshot");
                let scrolled = if screen == ActiveScreen::Main {
                    usize::from((cursor + 1).saturating_sub(3))
                } else {
                    0
                };
                assert_eq!(
                    small.surface.rows,
                    before.surface.rows[scrolled..scrolled + 3]
                );
                assert_eq!(small.surface.cursor.row, cursor.min(2));
                assert_eq!(small.surface.cursor.column, 3);
                assert_eq!(small.surface.cursor.visible, visible);

                model.resize(TerminalSize::new(6, 16)).expect("grow");
                let grown = model.snapshot();
                grown.validate().expect("grown snapshot");
                assert_eq!(
                    grown.surface.rows[..scrolled + 3],
                    before.surface.rows[..scrolled + 3]
                );
                for row in &grown.surface.rows[scrolled + 3..] {
                    assert!(row.cells.iter().all(|cell| cell.contents.trim().is_empty()));
                }
            }
        }
    }
}

#[test]
fn alternate_resize_clamps_current_and_saved_cursors_and_answers_immediately() {
    let mut model = TerminalModel::new(TerminalSize::new(6, 16), 32).expect("model");
    model
        .ingest(b"\x1b[?1049h\x1b[6;16H\x1b7\x1b[6;3H")
        .expect("save bottom-right cursor");
    model
        .resize(TerminalSize::new(3, 10))
        .expect("shrink both axes");
    let query = model
        .ingest(b"\x1b[6n\x1b[18t")
        .expect("size and position queries");
    assert_eq!(query.replies, b"\x1b[3;3R\x1b[8;3;10t");
    model.resize(TerminalSize::new(6, 16)).expect("grow again");
    let query = model.ingest(b"\x1b8\x1b[6n").expect("restore saved cursor");
    assert_eq!(query.replies, b"\x1b[3;10R");
    assert_eq!(model.presentation_deadline(), None);
}

#[test]
fn alternate_repaint_after_resize_still_patches_only_changed_rows() {
    let mut model = TerminalModel::new(TerminalSize::new(6, 16), 32).expect("model");
    model
        .ingest(b"\x1b[?1049hheader\x1b[2;1Hbody\x1b[6;1Hfooter")
        .expect("full screen");
    model.resize(TerminalSize::new(3, 16)).expect("shrink");
    let checkpoint = model.checkpoint();
    let mut applied = model.snapshot();
    model
        .ingest(b"\x1b[3;1Hfooter")
        .expect("child moves only its footer");
    let TerminalSurfaceDeltaResult::Delta(delta) = model.delta_or_resync(&checkpoint) else {
        panic!("same-size repaint must remain a delta");
    };
    assert_eq!(delta.row_patches.len(), 1);
    delta
        .apply_to(applied.revision, &mut applied.surface)
        .expect("row patch");
    assert_eq!(applied.surface, model.snapshot().surface);
}
