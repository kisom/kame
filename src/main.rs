//! kame — a distraction-free editor for long-form writing.
//!
//! The binary opens (or creates) one `.kame` SQLite file, loads its document,
//! lays every block out with cosmic-text, rasterizes each block to an egui
//! texture, and paints the result. Keyboard input is translated into editor
//! commands; every command is persisted through the durable op log so history
//! survives a restart.

mod edit;
mod export;
mod layout;
mod model;
mod render;
mod storage;

use std::error::Error;
use std::path::{Path, PathBuf};

use eframe::egui;
use egui::{Color32, Pos2, Rect, Sense, Vec2};

use crate::edit::{Cursor, EditError, Editor, Op, Selection};
use crate::layout::{anchor_to_y, BlockLayout, LayoutConfig};
use crate::model::{Block, Document};
use crate::render::egui_render::{
    caret_line, caret_x, command_for_event, next_boundary, offset_at, prev_boundary, Command,
    GlyphRenderer, TextureCache,
};
use crate::storage::{Db, DbError};

/// Paper-coloured canvas behind the page.
const BACKGROUND: Color32 = Color32::from_rgb(247, 243, 235);
/// Warm near-black ink used for rasterized glyphs.
const BODY_INK: [u8; 3] = [32, 29, 25];
/// Caret colour: a muted vermilion that reads against both faces.
const CARET_INK: Color32 = Color32::from_rgb(176, 58, 46);
/// Space above the first block and below the last.
const PAGE_PADDING: f32 = 36.0;
/// Vertical space between blocks.
const BLOCK_GAP: f32 = 14.0;

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn main() -> eframe::Result {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: kame <file.kame>");
        std::process::exit(2);
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 860.0])
            .with_min_inner_size([560.0, 360.0])
            .with_title("kame"),
        ..Default::default()
    };

    eframe::run_native(
        "kame",
        native_options,
        Box::new(move |cc| Ok(Box::new(KameApp::new(cc, path)?))),
    )
}

// --- session ---------------------------------------------------------------

/// The document, its durable history, and the caret — everything that exists
/// without a window.
///
/// Every mutation goes through one of the command methods, which apply the
/// edit, persist it, and move the caret. That keeps the GUI layer free of
/// persistence rules and lets the whole editing loop be tested headlessly.
struct Session {
    editor: Editor,
    db: Db,
    caret: Cursor,
    last_error: Option<String>,
}

impl Session {
    /// Opens the database at `path`, creating a fresh document when it holds
    /// none yet, and resumes the editor at the persisted undo cursor.
    fn open(path: &Path) -> AppResult<Session> {
        let mut db = Db::open(path)?;
        let (document, ops, cursor) = match db.load_document() {
            Ok(document) => {
                let ops = db.load_ops()?.into_iter().map(|(_, op)| op).collect();
                let cursor = db.cursor()?;
                (document, ops, cursor)
            }
            Err(DbError::NoDocument) => {
                let title = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Untitled");
                let document = Document::new(title);
                db.save_document(&document)?;
                (document, Vec::new(), 0)
            }
            Err(err) => return Err(Box::new(err)),
        };

        // Resume with the persisted cursor, never with_history: the saved
        // head may already have ops undone, and treating the whole log as
        // applied would offer those ops for undo a second time.
        let editor = Editor::with_cursor(document, ops, cursor);
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        Ok(Session {
            editor,
            db,
            caret,
            last_error: None,
        })
    }

    fn document(&self) -> &Document {
        self.editor.document()
    }

    fn history(&self) -> (usize, usize) {
        self.editor.history()
    }

    /// Persists the head document and the undo cursor together, the write
    /// path for undo/redo.
    fn save_state(&mut self) {
        if let Err(err) = self
            .db
            .save_state(self.editor.document(), self.editor.cursor())
        {
            self.last_error = Some(format!("save failed: {err}"));
        }
    }

    /// Persists one applied edit (op + head + cursor in one transaction).
    fn record(&mut self, op: &Op) {
        if let Err(err) = self.db.record_edit(self.editor.document(), op) {
            self.last_error = Some(format!("save failed: {err}"));
        }
    }

    /// Applies `result` from an editor command and persists it.
    fn commit(&mut self, result: Result<Op, EditError>) -> bool {
        match result {
            Ok(op) => {
                self.record(&op);
                true
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                false
            }
        }
    }

    /// The caret, repaired against the current document (an undo can remove
    /// the block it points into).
    fn valid_caret(&mut self) -> Cursor {
        self.caret = clamp_cursor(self.editor.document(), self.caret);
        self.caret
    }

    fn set_caret(&mut self, caret: Cursor) {
        self.caret = clamp_cursor(self.editor.document(), caret);
    }

    fn block_index(&self, block_id: crate::model::BlockId) -> Option<usize> {
        self.editor
            .document()
            .blocks
            .iter()
            .position(|block| block.id == block_id)
    }

    // --- editing commands --------------------------------------------------

    fn insert(&mut self, text: &str) -> bool {
        let caret = self.valid_caret();
        match self.editor.insert_text(&caret, text) {
            Ok(op) => {
                self.record(&op);
                self.caret = Cursor {
                    block_id: caret.block_id,
                    offset: caret.offset + text.len(),
                };
                true
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                false
            }
        }
    }

    /// Deletes `selection` and parks the caret at `caret_after`.
    fn delete_range(&mut self, selection: Selection, caret_after: Cursor) -> bool {
        let result = self.editor.delete_selection(&selection);
        if self.commit(result) {
            self.caret = caret_after;
            true
        } else {
            false
        }
    }

    fn backspace(&mut self) -> bool {
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };

        let previous = {
            let document = self.editor.document();
            prev_boundary(&document.blocks[index].text, caret.offset)
        };
        if let Some(offset) = previous {
            let target = Cursor {
                block_id: caret.block_id,
                offset,
            };
            return self.delete_range(
                Selection {
                    start: target,
                    end: caret,
                },
                target,
            );
        }
        if index == 0 {
            return false;
        }

        // At the start of a block: join it to the previous one.
        let (previous_id, previous_len) = {
            let document = self.editor.document();
            let previous = &document.blocks[index - 1];
            (previous.id, previous.text.len())
        };
        let at_end = Cursor {
            block_id: previous_id,
            offset: previous_len,
        };
        let result = self.editor.merge_with_next(&at_end);
        if self.commit(result) {
            self.caret = at_end;
            true
        } else {
            false
        }
    }

    fn delete_forward(&mut self) -> bool {
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };

        let next = {
            let document = self.editor.document();
            next_boundary(&document.blocks[index].text, caret.offset)
        };
        if let Some(offset) = next {
            let end = Cursor {
                block_id: caret.block_id,
                offset,
            };
            return self.delete_range(Selection { start: caret, end }, caret);
        }
        if index + 1 >= self.editor.document().blocks.len() {
            return false;
        }
        // At the end of a block: pull the next block's text up.
        let result = self.editor.merge_with_next(&caret);
        self.commit(result)
    }

    fn split(&mut self) -> bool {
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };
        let result = self.editor.split_paragraph(&caret);
        if !self.commit(result) {
            return false;
        }
        if let Some(next) = self.editor.document().blocks.get(index + 1) {
            self.caret = Cursor {
                block_id: next.id,
                offset: 0,
            };
            true
        } else {
            false
        }
    }

    /// Inserts pasted text one line per block: each newline becomes a
    /// paragraph break, so the block model keeps its one-paragraph invariant.
    fn paste(&mut self, text: &str) -> bool {
        let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut changed = false;
        for (index, line) in normalised.split('\n').enumerate() {
            if index > 0 {
                changed |= self.split();
            }
            if !line.is_empty() {
                changed |= self.insert(line);
            }
        }
        changed
    }

    fn undo(&mut self) -> bool {
        if self.editor.undo().is_none() {
            return false;
        }
        self.save_state();
        self.caret = clamp_cursor(self.editor.document(), self.caret);
        true
    }

    fn redo(&mut self) -> bool {
        if self.editor.redo().is_none() {
            return false;
        }
        self.save_state();
        self.caret = clamp_cursor(self.editor.document(), self.caret);
        true
    }

    fn move_left(&mut self) {
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match prev_boundary(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None if index > 0 => {
                    let previous = &document.blocks[index - 1];
                    Cursor {
                        block_id: previous.id,
                        offset: previous.text.len(),
                    }
                }
                None => caret,
            }
        };
        self.caret = target;
    }

    fn move_right(&mut self) {
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match next_boundary(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None => match document.blocks.get(index + 1) {
                    Some(next) => Cursor {
                        block_id: next.id,
                        offset: 0,
                    },
                    None => caret,
                },
            }
        };
        self.caret = target;
    }
}

/// Clamps a caret to a block that exists and to a character boundary inside it.
fn clamp_cursor(document: &Document, cursor: Cursor) -> Cursor {
    match document
        .blocks
        .iter()
        .find(|block| block.id == cursor.block_id)
    {
        Some(block) => {
            let mut offset = cursor.offset.min(block.text.len());
            while offset > 0 && !block.text.is_char_boundary(offset) {
                offset -= 1;
            }
            Cursor {
                block_id: block.id,
                offset,
            }
        }
        None => document
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .unwrap_or(cursor),
    }
}

/// Maps a click to a caret inside the block under it. A click past the last
/// block lands at the end of the document.
fn hit_test(
    blocks: &[Block],
    layouts: &[BlockLayout],
    tops: &[f32],
    heights: &[f32],
    body_left: f32,
    pos: Pos2,
) -> Option<Cursor> {
    for (index, block) in blocks.iter().enumerate() {
        let top = tops[index];
        if pos.y >= top && pos.y <= top + heights[index] {
            let offset = offset_at(
                &layouts[index],
                pos.x - body_left,
                pos.y - top,
                block.text.len(),
            );
            return Some(Cursor {
                block_id: block.id,
                offset,
            });
        }
    }
    blocks.last().map(|block| Cursor {
        block_id: block.id,
        offset: block.text.len(),
    })
}

/// Minimum height of a block, so an empty paragraph still has a line to click
/// and a caret to show.
fn block_height(block: &Block, layout: &BlockLayout, config: &LayoutConfig) -> f32 {
    let minimum = match block.kind {
        crate::model::BlockKind::Heading { .. } => config.heading_size * 1.4,
        _ => config.body_line_height,
    };
    layout.height.max(minimum)
}

// --- eframe application ----------------------------------------------------

struct KameApp {
    session: Session,
    renderer: GlyphRenderer,
    textures: TextureCache,
    path: PathBuf,
}

impl KameApp {
    fn new(cc: &eframe::CreationContext<'_>, path: PathBuf) -> AppResult<KameApp> {
        let session = Session::open(&path)?;
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        Ok(KameApp {
            session,
            renderer: GlyphRenderer::new(LayoutConfig::default()),
            textures: TextureCache::new(),
            path,
        })
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|input| input.events.clone());
        for event in &events {
            let Some(command) = command_for_event(event) else {
                continue;
            };
            match command {
                Command::Insert(text) => {
                    self.session.insert(&text);
                }
                Command::Paste(text) => {
                    self.session.paste(&text);
                }
                Command::Backspace => {
                    self.session.backspace();
                }
                Command::Delete => {
                    self.session.delete_forward();
                }
                Command::SplitParagraph => {
                    self.session.split();
                }
                Command::Undo => {
                    self.session.undo();
                }
                Command::Redo => {
                    self.session.redo();
                }
                Command::MoveLeft => self.session.move_left(),
                Command::MoveRight => self.session.move_right(),
                Command::MoveUp => self.move_vertically(false),
                Command::MoveDown => self.move_vertically(true),
                Command::MoveLineStart => self.move_to_line_edge(false),
                Command::MoveLineEnd => self.move_to_line_edge(true),
            }
        }
    }

    /// Up/Down move by visual line inside the current block, keeping the
    /// caret's horizontal position. Crossing into another block is left to
    /// Left/Right in v1.
    fn move_vertically(&mut self, down: bool) {
        let caret = self.session.valid_caret();
        let Some(index) = self.session.block_index(caret.block_id) else {
            return;
        };
        let block = self.session.document().blocks[index].clone();
        let layout = self.renderer.layout(&block);
        let Some(line) = caret_line(&layout, caret.offset) else {
            return;
        };
        let Some(line_index) = layout
            .lines
            .iter()
            .position(|candidate| candidate.y == line.y)
        else {
            return;
        };
        let target = if down {
            layout.lines.get(line_index + 1)
        } else {
            line_index
                .checked_sub(1)
                .and_then(|previous| layout.lines.get(previous))
        };
        let Some(target) = target else {
            return;
        };
        let x = caret_x(&layout, caret.offset);
        let offset = offset_at(&layout, x, target.y + 1.0, block.text.len());
        self.session.set_caret(Cursor {
            block_id: block.id,
            offset,
        });
    }

    fn move_to_line_edge(&mut self, end: bool) {
        let caret = self.session.valid_caret();
        let Some(index) = self.session.block_index(caret.block_id) else {
            return;
        };
        let block = self.session.document().blocks[index].clone();
        let layout = self.renderer.layout(&block);
        let Some(line) = caret_line(&layout, caret.offset) else {
            return;
        };
        let offset = if end { line.end } else { line.start };
        self.session.set_caret(Cursor {
            block_id: block.id,
            offset,
        });
    }

    fn show_status(&mut self, ui: &mut egui::Ui) {
        let title = self.session.document().title.clone();
        let blocks = self.session.document().blocks.len();
        let (undo, redo) = self.session.history();
        let path = self.path.display().to_string();
        let error = self.session.last_error.clone();

        egui::Panel::top("kame-status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(title);
                ui.separator();
                ui.label(format!(
                    "{blocks} block{}",
                    if blocks == 1 { "" } else { "s" }
                ));
                ui.label(format!("undo {undo} · redo {redo}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(path);
                });
            });
            ui.label(
                egui::RichText::new(
                    "Type to write · Enter new paragraph · Backspace/Delete · \
                     Ctrl-Z undo · Ctrl-Shift-Z redo · click to place the caret",
                )
                .small()
                .weak(),
            );
            if let Some(error) = error {
                ui.colored_label(Color32::from_rgb(168, 32, 32), error);
            }
        });
    }

    fn draw_surface(&mut self, ui: &mut egui::Ui) {
        let config = self.renderer.config();

        // Shape every block once for this frame; the layouts drive both the
        // texture cache and caret/click mapping.
        let mut layouts: Vec<BlockLayout> = Vec::new();
        {
            let document = self.session.document();
            for block in &document.blocks {
                layouts.push(self.renderer.layout(block));
            }
        }

        self.textures.sync(
            ui.ctx(),
            &mut self.renderer,
            &self.session.document().blocks,
            &layouts,
            BODY_INK,
        );

        let blocks = &self.session.document().blocks;
        let caret = self.session.caret;
        let heights: Vec<f32> = blocks
            .iter()
            .zip(layouts.iter())
            .map(|(block, layout)| block_height(block, layout, &config))
            .collect();

        let page_width = config.body_width + 2.0 * config.margin_width;
        let content_height = (PAGE_PADDING * 2.0
            + heights.iter().sum::<f32>()
            + BLOCK_GAP * blocks.len().saturating_sub(1) as f32)
            .max(ui.available_height());

        let (response, painter) = ui.allocate_painter(
            Vec2::new(ui.available_width(), content_height),
            Sense::click(),
        );
        let page_left =
            response.rect.left() + ((response.rect.width() - page_width) * 0.5).max(0.0);
        let body_left = page_left + config.margin_width;

        let mut tops = Vec::with_capacity(blocks.len());
        let mut y = response.rect.top() + PAGE_PADDING;
        for height in &heights {
            tops.push(y);
            y += height + BLOCK_GAP;
        }

        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for (index, block) in blocks.iter().enumerate() {
            if let Some(texture) = self.textures.get(block.id) {
                let origin = Pos2::new(body_left, tops[index]);
                painter.image(
                    texture.texture.id(),
                    texture.rect(origin),
                    uv,
                    Color32::WHITE,
                );
            }
        }

        let mut clicked_caret = None;

        if let Some(index) = blocks.iter().position(|block| block.id == caret.block_id) {
            let line_y = anchor_to_y(blocks, &layouts, caret.block_id, caret.offset).unwrap_or(0.0);
            let line_height = layouts[index]
                .lines
                .iter()
                .find(|line| (line.y - line_y).abs() < 0.5)
                .map_or(config.body_line_height, |line| line.height);
            let x = caret_x(&layouts[index], caret.offset);
            let top = tops[index] + line_y;
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(body_left + x, top), Vec2::new(1.5, line_height)),
                0.0,
                CARET_INK,
            );
        }

        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                clicked_caret = hit_test(blocks, &layouts, &tops, &heights, body_left, pos);
            }
        }

        // `blocks` borrows the document; the caret move is applied after that
        // borrow's last use.
        if let Some(caret) = clicked_caret {
            self.session.set_caret(caret);
        }
    }
}

impl eframe::App for KameApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_input(ui.ctx());
        self.show_status(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.draw_surface(ui));
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kame-gui-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("doc.kame")
    }

    fn open(path: &Path) -> Session {
        Session::open(path).expect("open a session")
    }

    fn first_block_id(session: &Session) -> crate::model::BlockId {
        session.document().blocks[0].id
    }

    #[test]
    fn fresh_file_creates_a_document() {
        let path = temp_db_path();
        let session = open(&path);
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "");
        assert_eq!(session.document().title, "doc");
        assert_eq!(session.caret.offset, 0);
        assert!(path.exists(), "opening creates the .kame file");
    }

    #[test]
    fn typing_persists_and_the_caret_advances() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert("Hello"));
        assert!(session.insert(" world"));
        assert_eq!(session.document().blocks[0].text, "Hello world");
        assert_eq!(session.caret.offset, 11);
        assert_eq!(session.history(), (2, 0));
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "Hello world");
        assert_eq!(reopened.history(), (2, 0));
        // Undo after reopening must undo one insert, not re-apply the log.
        assert!(reopened.undo());
        assert_eq!(reopened.document().blocks[0].text, "Hello");
        assert_eq!(reopened.history(), (1, 1));
    }

    #[test]
    fn split_creates_a_paragraph_and_puts_the_caret_in_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one two");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });
        assert!(session.split());

        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(session.document().blocks[0].text, "one");
        assert_eq!(session.document().blocks[1].text, " two");
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
        assert_eq!(session.caret.offset, 0);

        session.insert("new");
        assert_eq!(session.document().blocks[1].text, "new two");
        assert_eq!(session.caret.offset, 3);
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().blocks.len(), 2);
        assert_eq!(reopened.document().blocks[1].text, "new two");
    }

    #[test]
    fn backspace_at_a_block_start_joins_the_previous_paragraph() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("first");
        session.split();
        session.insert("second");
        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(session.caret.offset, 6);

        assert!(session.backspace());
        assert_eq!(session.document().blocks[1].text, "secon");
        assert_eq!(session.caret.offset, 5);

        session.set_caret(Cursor {
            block_id: session.caret.block_id,
            offset: 0,
        });
        assert!(session.backspace());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "firstsecon");
        assert_eq!(session.caret.offset, 5, "caret lands at the join");
        assert_eq!(session.caret.block_id, first_block_id(&session));
    }

    #[test]
    fn delete_at_a_block_end_pulls_the_next_paragraph_up() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one");
        session.split();
        session.insert("two");
        session.set_caret(Cursor {
            block_id: first_block_id(&session),
            offset: 3,
        });
        assert!(session.delete_forward());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "onetwo");
        assert_eq!(session.caret.offset, 3);
    }

    #[test]
    fn backspace_deletes_a_whole_utf8_character() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("aé");
        assert_eq!(session.caret.offset, 3);
        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "a");
        assert_eq!(session.caret.offset, 1);
    }

    #[test]
    fn undo_and_redo_persist_head_and_cursor_across_a_reopen() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("ab");
        session.insert("cd");
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "ab");
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "ab");
        assert_eq!(reopened.history(), (1, 1));
        assert!(reopened.redo());
        assert_eq!(reopened.document().blocks[0].text, "abcd");
        assert_eq!(reopened.history(), (2, 0));
    }

    #[test]
    fn paste_splits_crlf_text_into_paragraphs() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.paste("one\r\ntwo\r\nthree"));
        let texts: Vec<&str> = session
            .document()
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
        assert_eq!(session.caret.offset, 5);
        assert_eq!(session.caret.block_id, session.document().blocks[2].id);
    }

    #[test]
    fn arrows_cross_block_boundaries() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("ab");
        session.split();
        session.insert("cd");
        session.set_caret(Cursor {
            block_id: session.caret.block_id,
            offset: 0,
        });
        session.move_left();
        assert_eq!(session.caret.offset, 2);
        assert_eq!(session.caret.block_id, first_block_id(&session));
        session.move_right();
        assert_eq!(session.caret.offset, 0);
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
    }

    #[test]
    fn clamping_repairs_a_stale_caret_after_undo() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        session.split();
        let second = session.document().blocks[1].id;
        session.insert("ghost");
        session.set_caret(Cursor {
            block_id: second,
            offset: 5,
        });
        assert!(session.undo()); // removes "ghost"
        assert_eq!(session.caret.block_id, second);
        assert_eq!(session.caret.offset, 0, "offset clamps to the empty block");
        // Undo the split as well: the block disappears and the caret moves home.
        assert!(session.undo());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.caret.block_id, first_block_id(&session));
        assert_eq!(session.caret.offset, 0);
    }
}
