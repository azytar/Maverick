// maverick/src/backend/bar.rs
// Status bar rendered with plain X11 GC (image_text8 / poly_fill_rectangle).
// No deps beyond x11rb. Layout: [monitor] [tags] | layout | title ... | status
//
// Modern flat style, zero unsafe, zero extra deps:
//   - accent underline (brighter on the active monitor)
//   - pill-style active tag + occupied dot + urgent highlight
//   - active-monitor marker
//   - smart "…" title truncation
//   - subtle separators
//
// Performance: all GC changes and draw calls are fire-and-forget (no .check()).
// A single flush() at the end of the event loop batches everything into one
// round-trip.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::config::Cfg;
use crate::types::State;

// Bitmap font — the only font we can render without an Xft/FreeType dependency.
// Kept at 13px so char widths stay predictable for layout math.
#[allow(non_snake_case)]
const FONT: &[u8] = b"-misc-fixed-medium-r-normal--13-120-75-75-c-70-iso8859-1\0";

pub(crate) const TAG_PAD: i16 = 10;
const SEP_W: u16 = 6;
const ACCENT_H: u16 = 2; // bottom accent bar height in px
const START_X: i16 = 9; // leave room for the active-monitor marker

// Catppuccin Mocha accents used for chrome (kept local so draw() reads clearly).
const COL_SEP: u32 = 0x313244; // mantle — separators
const COL_OCC: u32 = 0xa6e3a1; // green  — occupied dot
const COL_STATUS: u32 = 0xa6adc8; // subtle text
const COL_SURFACE2: u32 = 0x585b70; // dim text for empty tags

pub struct Bar {
    pub font_id: u32,
    pub font_ascent: i32,
    pub char_w: u32, // approximate average char width for monospace
}

impl Bar {
    pub fn load(conn: &RustConnection) -> Result<Self, Box<dyn std::error::Error>> {
        let font_id = conn.generate_id()?;
        // Preferred font first; fall back to the server's built-in "fixed".
        let _ = conn.open_font(font_id, FONT);
        if conn.query_font(font_id)?.reply().is_err() {
            let _ = conn.open_font(font_id, b"fixed\0");
        }

        let fi = conn.query_font(font_id)?.reply()?;
        let font_ascent = fi.font_ascent as i32;
        let char_w = fi.min_bounds.character_width.unsigned_abs() as u32;
        let char_w = if char_w == 0 { 7 } else { char_w };

        Ok(Self {
            font_id,
            font_ascent,
            char_w,
        })
    }

    pub fn draw(
        &self,
        conn: &RustConnection,
        state: &State,
        mon_idx: usize,
        cfg: &Cfg,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &state.monitors[mon_idx];
        let bar_win = match mon.bar_win {
            Some(w) => w,
            None => return Ok(()),
        };
        let gc = match mon.bar_gc {
            Some(g) => g,
            None => return Ok(()),
        };
        if !mon.show_bar {
            return Ok(());
        }

        let bar_w = mon.screen.w as u16;
        let bar_h = cfg.bar_height as u16;
        let text_y = (bar_h as i32 / 2 + self.font_ascent / 2) as i16;
        let is_active_mon = state.sel_mon == mon_idx;

        // ── clear background ── (fire-and-forget; batched by the loop's flush)
        let _ = conn.change_gc(
            gc,
            &ChangeGCAux::new()
                .foreground(cfg.col_bar_bg)
                .background(cfg.col_bar_bg),
        );
        let _ = conn.poly_fill_rectangle(
            bar_win,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: bar_w,
                height: bar_h,
            }],
        );

        // ── active-monitor marker ──
        if is_active_mon {
            let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(cfg.col_bar_sel));
            let _ = conn.poly_fill_rectangle(
                bar_win,
                gc,
                &[Rectangle {
                    x: 2,
                    y: bar_h as i16 / 2 - 4,
                    width: 3,
                    height: 8,
                }],
            );
        }

        let mut x: i16 = START_X;

        // ── workspace tags ──
        for (i, ws) in mon.workspaces.iter().enumerate() {
            let name = cfg.tag_names.get(i).unwrap_or(&"?");
            let is_active = i == mon.active_ws;
            let is_occupied = !ws.is_empty();
            let has_urgent = ws
                .columns
                .iter()
                .flat_map(|c| &c.windows)
                .chain(ws.floats.iter())
                .any(|&w| {
                    state
                        .clients
                        .get(&w)
                        .is_some_and(|c| c.flags.has(crate::types::WinFlags::URGENT))
                });

            let (bg, fg, draw_bg) = if is_active {
                (cfg.col_bar_sel, cfg.col_bar_bg, true)
            } else if has_urgent {
                (cfg.col_urgent, cfg.col_bar_bg, true)
            } else if is_occupied {
                (cfg.col_bar_bg, COL_OCC, false)
            } else {
                (cfg.col_bar_bg, COL_SURFACE2, false)
            };

            let label = to_latin1(name, 255);
            let label_w = (label.len() as u16) * (self.char_w as u16) + TAG_PAD as u16 * 2;

            // pill background for active / urgent tags
            if draw_bg {
                let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(bg));
                let _ = conn.poly_fill_rectangle(
                    bar_win,
                    gc,
                    &[Rectangle {
                        x,
                        y: 2,
                        width: label_w,
                        height: bar_h - 4,
                    }],
                );
            }

            let _ = conn
                .change_gc(gc, &ChangeGCAux::new().foreground(fg).background(cfg.col_bar_bg));
            let _ = conn.image_text8(bar_win, gc, x + TAG_PAD, text_y, &label);

            // occupied dot for non-active, non-urgent tags
            if is_occupied && !is_active && !has_urgent {
                let dot_x = x + TAG_PAD + label.len() as i16 * self.char_w as i16 + 2;
                let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(COL_OCC));
                let _ = conn.poly_fill_rectangle(
                    bar_win,
                    gc,
                    &[Rectangle {
                        x: dot_x,
                        y: bar_h as i16 / 2 - 1,
                        width: 3,
                        height: 3,
                    }],
                );
            }

            x += label_w as i16 + 2;
        }

        // ── separator ──
        x = self.separator(conn, gc, bar_win, x, bar_h);
        x += SEP_W as i16;

        // ── layout symbol (per-workspace) ──
        let layout_sym = mon.ws().layout.symbol();
        let _ = conn.change_gc(
            gc,
            &ChangeGCAux::new()
                .foreground(COL_LAYOUT_CYAN)
                .background(cfg.col_bar_bg),
        );
        let _ = conn.image_text8(bar_win, gc, x, text_y, layout_sym.as_bytes());
        x += (layout_sym.len() as i16) * self.char_w as i16 + SEP_W as i16;

        // ── separator ──
        x = self.separator(conn, gc, bar_win, x, bar_h);
        x += SEP_W as i16;

        // ── status text (right-aligned) ──
        let status_l1 = truncate_latin1(&state.status, 255);
        let status_w = (status_l1.len() as u16) * (self.char_w as u16) + 8;
        let status_x = bar_w.saturating_sub(status_w) as i16;

        if status_x > x + 8 && !status_l1.is_empty() {
            let _ = conn.change_gc(
                gc,
                &ChangeGCAux::new()
                    .foreground(COL_STATUS)
                    .background(cfg.col_bar_bg),
            );
            let _ = conn.image_text8(bar_win, gc, status_x, text_y, &status_l1);
        }

        // ── focused window title (between current x and status) ──
        if let Some(focused) = mon.focused {
            if let Some(client) = state.clients.get(&focused) {
                let avail_glyphs =
                    ((status_x - x - 4).max(0) as usize / self.char_w as usize).min(255);
                if avail_glyphs > 1 {
                    let title_l1 = truncate_latin1(&client.name, avail_glyphs);
                    if !title_l1.is_empty() {
                        let _ = conn.change_gc(
                            gc,
                            &ChangeGCAux::new()
                                .foreground(cfg.col_bar_fg)
                                .background(cfg.col_bar_bg),
                        );
                        let _ = conn.image_text8(bar_win, gc, x, text_y, &title_l1);
                    }
                }
            }
        }

        // ── accent underline (brighter on the active monitor) ──
        let accent = if is_active_mon {
            cfg.col_bar_sel
        } else {
            COL_SEP
        };
        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(accent));
        let _ = conn.poly_fill_rectangle(
            bar_win,
            gc,
            &[Rectangle {
                x: 0,
                y: bar_h as i16 - ACCENT_H as i16,
                width: bar_w,
                height: ACCENT_H,
            }],
        );

        // flush() is NOT called here — the event loop calls conn.flush() after
        // flush_bars() returns, batching all X11 output in one syscall.
        Ok(())
    }

    /// Draw a 1px vertical separator and return the x just past it.
    #[inline]
    fn separator(
        &self,
        conn: &RustConnection,
        gc: u32,
        bar_win: u32,
        x: i16,
        bar_h: u16,
    ) -> i16 {
        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(COL_SEP));
        let _ = conn.poly_fill_rectangle(
            bar_win,
            gc,
            &[Rectangle {
                x,
                y: 4,
                width: 1,
                height: bar_h - 8,
            }],
        );
        x + 1
    }

    /// Given a bar-relative x coordinate, return the workspace index that was clicked,
    /// or None if the click was outside all tag buttons. Mirrors `draw()` tag geometry exactly.
    pub fn tag_at_x(&self, x: i16, tag_names: &[&'static str]) -> Option<usize> {
        let mut cur_x: i16 = START_X;
        for (i, name) in tag_names.iter().enumerate() {
            // Mirrors draw()'s to_latin1() glyph count exactly: every char counts
            // toward width (non-Latin1 chars render as '?' but still take a slot).
            let label_w = tag_width(name, self.char_w);
            let right = cur_x + label_w + 2; // +2 is the inter-tag gap from draw()
            if x >= cur_x && x < right {
                return Some(i);
            }
            cur_x = right;
        }
        None
    }
}

const COL_LAYOUT_CYAN: u32 = 0x89dceb;

/// Width (px) of a tag label including its horizontal padding, matching `draw()`.
fn tag_width(name: &str, char_w: u32) -> i16 {
    let glyphs = to_latin1(name, 255).len() as i16;
    glyphs * char_w as i16 + TAG_PAD * 2
}

/// Convert a UTF-8 string to a Latin-1 byte vector for use with `image_text8`.
///
/// `image_text8` uses the X11 font encoding (ISO 8859-1 / Latin-1).  Raw UTF-8
/// bytes passed to it produce garbage because multi-byte sequences each render
/// as separate Latin-1 glyphs.  The correct approach is to convert code points:
///
///   U+0000..U+00FF  → same value as Latin-1 byte  (covers all European scripts)
///   U+0100+          → replaced with '?' (CJK, emoji, etc.)
///
/// `max_glyphs` is the maximum number of output bytes (= display characters).
/// `image_text8` has a hard CARD8 limit of 255 bytes, so always pass ≤ 255.
fn to_latin1(s: &str, max_glyphs: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(max_glyphs.min(s.len()));
    for ch in s.chars() {
        if out.len() >= max_glyphs {
            break;
        }
        let cp = ch as u32;
        out.push(if cp <= 0xFF { cp as u8 } else { b'?' });
    }
    out
}

/// Like `to_latin1` but appends "…" (rendered as "...") when the string is longer
/// than `max_glyphs`, so truncated titles stay readable.
fn truncate_latin1(s: &str, max_glyphs: usize) -> Vec<u8> {
    if s.chars().count() <= max_glyphs {
        return to_latin1(s, max_glyphs);
    }
    if max_glyphs <= 3 {
        return to_latin1(s, max_glyphs);
    }
    let mut out = to_latin1(s, max_glyphs - 3);
    out.extend_from_slice(b"...");
    out
}


