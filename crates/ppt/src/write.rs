//! A fresh, single-edit MS-PPT document. Simple OfficeArt shapes and Unicode text remain
//! editable; callers provide rendered PNGs for slides requiring unsupported drawing features.
//! Sources: MS-PPT sections 2.3–2.5 and 2.9, MS-ODRAW sections 2.2–2.3, MS-CFB.

use std::io::{Cursor, Write};

use deckcraft_color::Rgba;
use deckcraft_geom::Xfrm;
use deckcraft_model::resolve::{self, Ctx};
use deckcraft_model::text::{Align, Anchor, Bullet, Run, RunKind, TextBody};
use deckcraft_model::{Fill, Geom, Presentation, Shape, ShapeKind, Slide};

use crate::PptError;
use crate::doc::rt;
use crate::draw::prop;

type Result<T> = std::result::Result<T, PptError>;
const MAX_BYTES: usize = 256 << 20;
const MAX_SLIDES: usize = 1000;
const MAX_TEXT: usize = 1 << 20;
const MASTER: u32 = 0x8000_0000;

fn error(message: impl Into<String>) -> PptError {
    PptError::Write(message.into())
}
fn number(n: usize) -> Result<u32> {
    u32::try_from(n).map_err(|_| error("record is too large"))
}
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn record(version: u16, kind: u16, instance: u16, body: &[u8]) -> Result<Vec<u8>> {
    if body.len() > MAX_BYTES || instance > 4095 {
        return Err(error("record exceeds the export limit"));
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&(version | (instance << 4)).to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&number(body.len())?.to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
}
fn atom(kind: u16, body: &[u8]) -> Result<Vec<u8>> {
    record(0, kind, 0, body)
}
fn container(kind: u16, instance: u16, children: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for child in children {
        if child.len() > MAX_BYTES.saturating_sub(body.len()) {
            return Err(error("presentation exceeds 256 MiB"));
        }
        body.extend_from_slice(child);
    }
    record(15, kind, instance, &body)
}
fn options(entries: &[(u16, u32)]) -> Result<Vec<u8>> {
    let mut entries = entries.to_vec();
    entries.sort_by_key(|p| p.0 & 0x3fff);
    let mut body = Vec::new();
    for (key, value) in &entries {
        body.extend_from_slice(&key.to_le_bytes());
        body.extend_from_slice(&value.to_le_bytes());
    }
    record(3, rt::FOPT, u16::try_from(entries.len()).map_err(|_| error("too many shape properties"))?, &body)
}
fn color(c: Rgba) -> u32 {
    u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16)
}
fn coordinate(v: f64) -> Result<i16> {
    let n = (v * 8.0).round();
    if !n.is_finite() || n < f64::from(i16::MIN) || n > f64::from(i16::MAX) {
        return Err(error("shape coordinates exceed the legacy range"));
    }
    Ok(n as i16)
}
fn anchor(x: Xfrm) -> Result<Vec<u8>> {
    let points = [coordinate(x.y)?, coordinate(x.x)?, coordinate(x.x + x.w)?, coordinate(x.y + x.h)?];
    atom(rt::CLIENT_ANCHOR, &points.into_iter().flat_map(i16::to_le_bytes).collect::<Vec<_>>())
}
fn shape_type(shape: &Shape) -> Option<u16> {
    if let ShapeKind::Picture { .. } = shape.kind {
        return Some(75);
    }
    if !matches!(shape.kind, ShapeKind::Shape | ShapeKind::Connector { .. }) {
        return None;
    }
    let Geom::Preset { name, adj } = &shape.geom else { return None };
    if !adj.is_empty() {
        return None;
    }
    (1..=202).find(|&kind| crate::geom::preset_name(kind) == Some(name.as_str()))
}
fn simple_fill(fill: Option<&Fill>) -> bool {
    matches!(fill, None | Some(Fill::None | Fill::Solid { .. }))
}

fn spacing(value: Option<deckcraft_model::text::Spacing>) -> Result<i16> {
    let v = match value {
        Some(deckcraft_model::text::Spacing::Pct(v)) => v * 100.0,
        Some(deckcraft_model::text::Spacing::Pts(v)) => -v * 8.0,
        None => 0.0,
    };
    if !v.is_finite() || v < f64::from(i16::MIN) || v > f64::from(i16::MAX) {
        return Err(error("paragraph spacing exceeds the legacy range"));
    }
    Ok(v.round() as i16)
}

fn margin(value: f64) -> Result<u16> {
    let v = value * 8.0;
    if !v.is_finite() || !(0.0..=f64::from(u16::MAX)).contains(&v) {
        return Err(error("paragraph margin exceeds the legacy range"));
    }
    Ok(v.round() as u16)
}

fn simple_shape(ctx: &Ctx, s: &Shape) -> bool {
    if s.hidden {
        return true;
    }
    if shape_type(s).is_none() || anchor(resolve::xfrm(ctx, s)).is_err() {
        return false;
    }
    let (fill, _) = resolve::fill(ctx, s);
    let (line, _) = resolve::line(ctx, s);
    let (effects, _) = resolve::effects(ctx, s);
    if !simple_fill(fill.as_ref()) || !simple_fill(line.fill.as_ref()) || effects.is_some_and(|e| !e.is_empty()) {
        return false;
    }
    if line.head.is_some()
        || line.tail.is_some()
        || line.compound.is_some_and(|c| c != deckcraft_model::style::Compound::Single)
        || line.dash.as_ref().is_some_and(|d| !matches!(d, deckcraft_model::style::Dash::Preset { name } if name == "solid"))
    {
        return false;
    }
    if let ShapeKind::Picture { fill } = &s.kind {
        if fill.crop.iter().any(|v| *v != 0.0) {
            return false;
        }
        if !ctx.pres.media.iter().any(|m| m.id == fill.media && matches!(m.content_type.as_str(), "image/png" | "image/jpeg")) {
            return false;
        }
    }
    let body = resolve::body(ctx, s);
    if body.vert.is_some_and(|v| v != deckcraft_model::text::TextDir::Horizontal) {
        return false;
    }
    if let Some(text) = &s.text {
        for para in &text.paragraphs {
            let pp = resolve::para(ctx, s, para);
            if matches!(pp.bullet, Some(Bullet::AutoNum { .. } | Bullet::Picture { .. })) {
                return false;
            }
            if matches!(&pp.bullet, Some(Bullet::Char { char }) if char.encode_utf16().count() != 1) {
                return false;
            }
            if margin(pp.margin_left.unwrap_or(0.0)).is_err() || margin(pp.margin_left.unwrap_or(0.0) + pp.indent.unwrap_or(0.0)).is_err() {
                return false;
            }
            for run in &para.runs {
                let props = resolve::run(ctx, s, para, &run.props);
                if !matches!(run.kind, RunKind::Text | RunKind::Break)
                    || props.size.is_some_and(|v| v.fract() != 0.0)
                    || props.underline.as_deref().is_some_and(|v| !matches!(v, "none" | "sng"))
                    || props.baseline.unwrap_or(0.0) != 0.0
                    || props.spacing.unwrap_or(0.0) != 0.0
                    || !simple_fill(props.fill.as_ref())
                    || matches!(&props.fill, Some(Fill::Solid { color }) if ctx.color(color, None).a != 255)
                    || props.inner_shadow.is_some()
                    || props.highlight.is_some()
                    || props.outline.is_some()
                    || props.shadow.is_some()
                    || props.caps.is_some_and(|v| v != deckcraft_model::text::Caps::None)
                    || props.strike.is_some_and(|v| v != deckcraft_model::text::Strike::None)
                {
                    return false;
                }
            }
        }
    }
    body.warp.is_none() && body.rot.unwrap_or(0.0) == 0.0 && body.columns.unwrap_or(1) == 1
}

/// Whether this slide needs the caller's raster preview to preserve its visible appearance.
/// The writer never silently discards a shape it cannot serialize.
pub fn needs_preview(p: &Presentation, slide: &Slide) -> bool {
    let Some(ctx) = Ctx::for_slide(p, slide) else { return true };
    let (background, _) = resolve::background(&ctx, Some(slide));
    if !simple_fill(Some(&background)) {
        return true;
    }
    let mut count = slide.shapes.len();
    if !slide.shapes.iter().all(|s| simple_shape(&ctx, s)) {
        return true;
    }
    let (layout, master) = resolve::show_master_shapes(slide, ctx.layout);
    if master {
        let mctx = Ctx::for_master(p, ctx.master);
        for s in ctx.master.shapes.iter().filter(|s| s.ph.is_none()) {
            count += 1;
            if !simple_shape(&mctx, s) {
                return true;
            }
        }
    }
    if layout && let Some(l) = ctx.layout {
        let lctx = Ctx::for_layout(p, ctx.master, l);
        for s in l.shapes.iter().filter(|s| s.ph.is_none()) {
            count += 1;
            if !simple_shape(&lctx, s) {
                return true;
            }
        }
    }
    // One 1024-id OfficeArt cluster per drawing, including its patriarch and background.
    count > 1000
}

struct Writer {
    fonts: Vec<String>,
    pictures: Vec<Vec<u8>>,
    picture_stream: Vec<u8>,
    drawing_counts: Vec<u32>,
    bytes: usize,
}

impl Writer {
    fn font(&mut self, name: String) -> Result<u16> {
        if let Some(index) = self.fonts.iter().position(|s| *s == name) {
            return u16::try_from(index).map_err(|_| error("too many fonts"));
        }
        if self.fonts.len() >= 4095 {
            return Err(error("too many fonts"));
        }
        if name.encode_utf16().count() > 31 {
            return Err(error("font name is too long for PPT; use PPTX"));
        }
        let id = self.fonts.len() as u16;
        self.fonts.push(name);
        Ok(id)
    }
    fn picture(&mut self, bytes: &[u8], jpeg: bool) -> Result<u32> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.bytes) || self.pictures.len() >= 4095 {
            return Err(error("picture store exceeds the export limit"));
        }
        self.bytes += bytes.len();
        let index = number(self.pictures.len() + 1)?;
        let mut uid = [0u8; 16];
        if let Some(first) = uid.get_mut(..4) {
            first.copy_from_slice(&index.to_le_bytes());
        }
        let mut body = uid.to_vec();
        body.push(0xff);
        body.extend_from_slice(bytes);
        let blip = record(0, if jpeg { 0xf01d } else { 0xf01e }, if jpeg { 0x46a } else { 0x6e0 }, &body)?;
        let mut entry = vec![if jpeg { 5 } else { 6 }; 2];
        entry.extend_from_slice(&uid);
        entry.extend_from_slice(&0xffu16.to_le_bytes());
        // Legacy readers locate BLIPs by their byte offset in the separate Pictures stream.
        // Embedded FBSE payloads alone are insufficient for interoperable PPT picture lookup.
        entry.extend_from_slice(&words(&[number(blip.len())?, 1, number(self.picture_stream.len())?]));
        entry.extend_from_slice(&[0; 4]);
        self.picture_stream.extend_from_slice(&blip);
        self.pictures.push(record(2, rt::FBSE, if jpeg { 5 } else { 6 }, &entry)?);
        Ok(index)
    }
    fn text(&mut self, ctx: &Ctx, shape: &Shape, text: &TextBody) -> Result<Vec<u8>> {
        if text.paragraphs.len() > 10000 {
            return Err(error("too many paragraphs in a text box"));
        }
        let mut chars = Vec::new();
        let mut paragraph_styles = Vec::new();
        let mut character_styles = Vec::new();
        for (i, para) in text.paragraphs.iter().enumerate() {
            if para.runs.len() > 10000 {
                return Err(error("too many runs in a paragraph"));
            }
            let empty = Run::new("");
            let runs: Vec<&Run> = if para.runs.is_empty() { vec![&empty] } else { para.runs.iter().collect() };
            let mut para_chars = Vec::new();
            for (j, run) in runs.iter().enumerate() {
                let r = resolve::run(ctx, shape, para, &run.props);
                let content = if matches!(run.kind, RunKind::Break) { "\u{b}" } else { run.text.as_str() };
                let units: Vec<u16> = content.encode_utf16().take(MAX_TEXT + 1).map(|c| if c == 0 { 0xfffd } else { c }).collect();
                if units.len() > MAX_TEXT.saturating_sub(chars.len() + para_chars.len()) {
                    return Err(error("textbox exceeds one million UTF-16 units"));
                }
                para_chars.extend_from_slice(&units);
                let count = units.len() + usize::from(j + 1 == runs.len());
                let cost = units.len() * 2 + 32;
                if cost > MAX_BYTES.saturating_sub(self.bytes) {
                    return Err(error("text exceeds the export budget"));
                }
                self.bytes += cost;
                character_styles.extend_from_slice(&number(count)?.to_le_bytes());
                character_styles.extend_from_slice(&0x0007_0007u32.to_le_bytes());
                let flags = u16::from(r.bold == Some(true))
                    | (u16::from(r.italic == Some(true)) << 1)
                    | (u16::from(r.underline.as_deref().is_some_and(|s| s != "none")) << 2);
                character_styles.extend_from_slice(&flags.to_le_bytes());
                let font = ctx.master.theme.font(r.font.as_deref().unwrap_or("+mn-lt"));
                character_styles.extend_from_slice(&self.font(font)?.to_le_bytes());
                let size = r.size.unwrap_or(18.0);
                if !size.is_finite() || !(1.0..=4000.0).contains(&size) {
                    return Err(error("invalid font size"));
                }
                character_styles.extend_from_slice(&(size.round() as u16).to_le_bytes());
                let rgb = match &r.fill {
                    Some(Fill::Solid { color }) => ctx.color(color, None),
                    _ => Rgba::BLACK,
                };
                character_styles.extend_from_slice(&[rgb.r, rgb.g, rgb.b, 0xfe]);
            }
            let pp = resolve::para(ctx, shape, para);
            paragraph_styles.extend_from_slice(&number(para_chars.len() + 1)?.to_le_bytes());
            paragraph_styles.extend_from_slice(&u16::from(para.level.min(4)).to_le_bytes());
            let bullet = match &pp.bullet {
                Some(Bullet::Char { char }) => char.encode_utf16().next(),
                _ => None,
            };
            let bullet_font = pp.bullet_font.as_deref().filter(|s| !s.is_empty()).map(|font| self.font(ctx.master.theme.font(font))).transpose()?;
            let mask = 0x7d01u32 | if bullet.is_some() { 0x80 } else { 0 } | if bullet_font.is_some() { 0x12 } else { 0 };
            paragraph_styles.extend_from_slice(&mask.to_le_bytes());
            let flags = u16::from(bullet.is_some()) | (u16::from(bullet_font.is_some()) << 1);
            paragraph_styles.extend_from_slice(&flags.to_le_bytes());
            if let Some(c) = bullet {
                paragraph_styles.extend_from_slice(&c.to_le_bytes());
            }
            if let Some(font) = bullet_font {
                paragraph_styles.extend_from_slice(&font.to_le_bytes());
            }
            let align: u16 = match pp.align {
                Some(Align::Center) => 1,
                Some(Align::Right) => 2,
                Some(Align::Justify) => 3,
                _ => 0,
            };
            paragraph_styles.extend_from_slice(&align.to_le_bytes());
            paragraph_styles.extend_from_slice(&spacing(pp.line_spacing.or(Some(deckcraft_model::text::Spacing::Pct(1.0))))?.to_le_bytes());
            paragraph_styles.extend_from_slice(&spacing(pp.space_before)?.to_le_bytes());
            paragraph_styles.extend_from_slice(&spacing(pp.space_after)?.to_le_bytes());
            paragraph_styles.extend_from_slice(&margin(pp.margin_left.unwrap_or(0.0))?.to_le_bytes());
            paragraph_styles.extend_from_slice(&margin(pp.margin_left.unwrap_or(0.0) + pp.indent.unwrap_or(0.0))?.to_le_bytes());
            chars.extend(para_chars);
            if i + 1 < text.paragraphs.len() {
                chars.push(13);
            }
        }
        paragraph_styles.extend(character_styles);
        let mut children = vec![
            atom(rt::TEXT_HEADER, &4u32.to_le_bytes())?,
            atom(rt::TEXT_CHARS, &chars.into_iter().flat_map(u16::to_le_bytes).collect::<Vec<_>>())?,
        ];
        if !text.paragraphs.is_empty() {
            children.push(atom(rt::STYLE_TEXT_PROP, &paragraph_styles)?);
        }
        container(rt::CLIENT_TEXTBOX, 0, &children)
    }
    fn shape(&mut self, ctx: &Ctx, shape: &Shape, id: u32) -> Result<Vec<u8>> {
        let x = resolve::xfrm(ctx, shape);
        let kind = shape_type(shape).ok_or_else(|| error("unsupported shape needs a preview"))?;
        let flags = 0xa00 | (u32::from(x.flip_h) << 6) | (u32::from(x.flip_v) << 7);
        let mut props = Vec::new();
        let (fill, ph) = resolve::fill(ctx, shape);
        match fill {
            Some(Fill::Solid { color: c }) => {
                let c = ctx.color(&c, ph);
                props.extend([(prop::FILL_COLOR, color(c)), (prop::FILL_OPACITY, u32::from(c.a) * 65536 / 255), (prop::FILL_BOOLS, 0x0010_0010)]);
            }
            _ => props.push((prop::FILL_BOOLS, 0x0010_0000)),
        }
        let (line, ph) = resolve::line(ctx, shape);
        match line.fill {
            Some(Fill::Solid { color: c }) => {
                let c = ctx.color(&c, ph);
                props.extend([(prop::LINE_COLOR, color(c)), (prop::LINE_OPACITY, u32::from(c.a) * 65536 / 255), (prop::LINE_BOOLS, 0x0008_0008)]);
            }
            _ => props.push((prop::LINE_BOOLS, 0x0008_0000)),
        }
        if let Some(width) = line.width {
            if !width.is_finite() || !(0.0..=1000.0).contains(&width) {
                return Err(error("invalid line width"));
            }
            props.push((prop::LINE_WIDTH, (width * 12700.0).round() as u32));
        }
        if !x.rot.is_finite() {
            return Err(error("invalid rotation"));
        }
        props.push((prop::ROTATION, (((x.rot % 360.0) * 65536.0).round() as i32) as u32));
        let body = resolve::body(ctx, shape);
        for (key, value) in
            [(prop::TEXT_LEFT, body.inset_l), (prop::TEXT_TOP, body.inset_t), (prop::TEXT_RIGHT, body.inset_r), (prop::TEXT_BOTTOM, body.inset_b)]
        {
            let v = value.unwrap_or(0.0);
            if !v.is_finite() || v.abs() > 10000.0 {
                return Err(error("invalid text inset"));
            }
            props.push((key, ((v * 12700.0).round() as i32) as u32));
        }
        props.push((
            prop::ANCHOR_TEXT,
            match body.anchor {
                Some(Anchor::Middle) => 1,
                Some(Anchor::Bottom) => 2,
                _ => 0,
            },
        ));
        props.push((prop::WRAP_TEXT, if body.wrap == Some(false) { 2 } else { 0 }));
        if let ShapeKind::Picture { fill } = &shape.kind {
            let m = ctx.pres.media.iter().find(|m| m.id == fill.media).ok_or_else(|| error("picture data is missing"))?;
            props.push((prop::PIB | 0x4000, self.picture(&m.data, m.content_type == "image/jpeg")?));
        }
        let mut children = vec![record(2, rt::FSP, kind, &words(&[id, flags]))?, options(&props)?, anchor(x)?];
        if let Some(text) = &shape.text {
            children.push(self.text(ctx, shape, text)?);
        }
        container(rt::SP_CONTAINER, 0, &children)
    }
    fn drawing(&mut self, drawing: u32, shapes: Vec<Vec<u8>>, background: Option<Rgba>) -> Result<Vec<u8>> {
        let base = drawing * 1024;
        let patriarch = container(rt::SP_CONTAINER, 0, &[record(1, rt::FSPGR, 0, &[0; 16])?, record(2, rt::FSP, 0, &words(&[base, 5]))?])?;
        let count = number(shapes.len())? + 1 + u32::from(background.is_some());
        let mut tree = vec![patriarch];
        tree.extend(shapes);
        let mut children = vec![record(0, 0xf008, drawing as u16, &words(&[count, base + count - 1]))?, container(rt::SPGR_CONTAINER, 0, &tree)?];
        if let Some(c) = background {
            children.push(container(
                rt::SP_CONTAINER,
                0,
                &[
                    record(2, rt::FSP, 1, &words(&[base + count - 1, 0xe00]))?,
                    options(&[
                        (prop::FILL_COLOR, color(c)),
                        (prop::FILL_OPACITY, u32::from(c.a) * 65536 / 255),
                        (prop::FILL_BOOLS, 0x0010_0010),
                        (prop::LINE_BOOLS, 0x0008_0000),
                    ])?,
                ],
            )?);
        }
        self.drawing_counts.push(count);
        container(rt::DRAWING, 0, &[container(rt::DG_CONTAINER, 0, &children)?])
    }
    fn slide(&mut self, p: &Presentation, slide: &Slide, index: usize, preview: Option<&Vec<u8>>) -> Result<Vec<u8>> {
        let dg = 3 + number(index)? * 2;
        let mut shapes = Vec::new();
        let ctx = Ctx::for_slide(p, slide).ok_or_else(|| error("slide has no master"))?;
        let background = if let Some(png) = preview {
            let pid = self.picture(png, false)?;
            shapes.push(container(
                rt::SP_CONTAINER,
                0,
                &[
                    record(2, rt::FSP, 75, &words(&[dg * 1024 + 1, 0xa00]))?,
                    options(&[(prop::PIB | 0x4000, pid), (prop::FILL_BOOLS, 0x0010_0000), (prop::LINE_BOOLS, 0x0008_0000)])?,
                    anchor(Xfrm::new(0.0, 0.0, p.slide_size.width, p.slide_size.height))?,
                ],
            )?);
            Rgba::WHITE
        } else {
            if needs_preview(p, slide) {
                return Err(error("slide needs a rendered preview"));
            }
            let (layout, master) = resolve::show_master_shapes(slide, ctx.layout);
            let mut add = |ctx: &Ctx, items: &[Shape], inherited: bool| -> Result<()> {
                for s in items.iter().filter(|s| !s.hidden && (!inherited || s.ph.is_none())) {
                    shapes.push(self.shape(ctx, s, dg * 1024 + number(shapes.len())? + 1)?);
                }
                Ok(())
            };
            if master {
                add(&Ctx::for_master(p, ctx.master), &ctx.master.shapes, true)?;
            }
            if layout && let Some(l) = ctx.layout {
                add(&Ctx::for_layout(p, ctx.master, l), &l.shapes, true)?;
            }
            add(&ctx, &slide.shapes, false)?;
            let (fill, ph) = resolve::background(&ctx, Some(slide));
            match fill {
                Fill::Solid { color } => ctx.color(&color, ph),
                _ => Rgba::WHITE,
            }
        };
        let mut info = vec![0u8; 16];
        if let Some(hidden) = info.get_mut(10) {
            *hidden = if slide.hidden { 4 } else { 0 };
        }
        container(
            rt::SLIDE,
            0,
            &[
                slide_atom(MASTER, 256 + number(index)?)?,
                atom(rt::SLIDE_SHOW_SLIDE_INFO, &info)?,
                self.drawing(dg, shapes, Some(background))?,
                scheme()?,
            ],
        )
    }
    fn notes(&mut self, p: &Presentation, slide: &Slide, index: usize) -> Result<Vec<u8>> {
        let dg = 4 + number(index)? * 2;
        let ctx = Ctx::for_slide(p, slide).ok_or_else(|| error("slide has no master"))?;
        let mut shape =
            Shape { xfrm: Some(Xfrm::new(0.0, 0.0, 500.0, 650.0)), text: Some(slide.notes.clone()), text_box: true, ..Default::default() };
        // Notes body placeholder is required for legacy readers to recognize speaker notes.
        shape.fill = Some(Fill::None);
        let mut shape_bytes = self.shape(&ctx, &shape, dg * 1024 + 1)?;
        let ph = container(rt::CLIENT_DATA, 0, &[atom(rt::PLACEHOLDER_ATOM, &[0, 0, 0, 0, 0x0c, 0, 0, 0])?])?;
        // Rebuild the container so its length includes the placeholder client data.
        let data = shape_bytes.split_off(8);
        let mut data = data;
        data.extend(ph);
        let shape_bytes = record(15, rt::SP_CONTAINER, 0, &data)?;
        container(rt::NOTES, 0, &[atom(rt::NOTES_ATOM, &words(&[256 + number(index)?, 0]))?, self.drawing(dg, vec![shape_bytes], None)?, scheme()?])
    }
}

fn slide_atom(master: u32, notes: u32) -> Result<Vec<u8>> {
    let mut body = words(&[16, 0, 0, master, notes]);
    body.extend_from_slice(&[0; 4]);
    record(2, rt::SLIDE_ATOM, 0, &body)
}
fn scheme() -> Result<Vec<u8>> {
    record(0, rt::COLOR_SCHEME, 1, &words(&[0xffffff, 0, 0x808080, 0, 0xc0c0c0, 0x993333, 0x999900, 0x99cc00]))
}
fn persist(id: u32, slide: u32) -> Result<Vec<u8>> {
    atom(rt::SLIDE_PERSIST_ATOM, &words(&[id, 4, 0, slide, 0]))
}

fn default_style(kind: u16) -> Result<Vec<u8>> {
    // One inherited level; the writer sets each exported run's actual formatting explicitly.
    let mut body = 1u16.to_le_bytes().to_vec();
    body.extend(words(&[0, 0])); // empty paragraph and character exceptions
    record(0, rt::TEXT_MASTER_STYLE, kind, &body)
}

/// Write genuine binary `.ppt` bytes, never an OOXML archive with a legacy suffix.
/// `previews[i]` supplies a rendered PNG only for slides requiring unsupported visual features.
/// Limits prevent unbounded records, OfficeArt ids and compound-file allocations.
pub fn export(p: &Presentation, previews: &[Option<Vec<u8>>]) -> Result<Vec<u8>> {
    if p.slides.len() > MAX_SLIDES {
        return Err(error("PPT export is limited to 1000 slides; use PPTX for larger decks"));
    }
    for side in [p.slide_size.width, p.slide_size.height] {
        if !side.is_finite() || !(1.0..=4032.0).contains(&side) {
            return Err(error("invalid slide size"));
        }
    }
    let mut w = Writer { fonts: Vec::new(), pictures: Vec::new(), picture_stream: Vec::new(), drawing_counts: Vec::new(), bytes: 0 };
    w.font("Inter".into())?;
    let master = container(
        rt::MAIN_MASTER,
        0,
        &[slide_atom(0, 0)?, default_style(0)?, default_style(1)?, default_style(2)?, w.drawing(1, vec![], Some(Rgba::WHITE))?, scheme()?],
    )?;
    let notes_master = container(rt::NOTES, 0, &[atom(rt::NOTES_ATOM, &[0; 8])?, w.drawing(2, vec![], None)?, scheme()?])?;
    let mut pages = vec![master, notes_master];
    let mut slide_list = Vec::new();
    let mut notes_list = Vec::new();
    for (i, slide) in p.slides.iter().enumerate() {
        let preview = previews.get(i).and_then(Option::as_ref);
        pages.push(w.slide(p, slide, i, preview)?);
        pages.push(w.notes(p, slide, i)?);
        slide_list.push(persist(4 + number(i)? * 2, 256 + number(i)?)?);
        notes_list.push(persist(5 + number(i)? * 2, 256 + number(i)?)?);
    }
    let mut fonts = Vec::new();
    for (i, name) in w.fonts.iter().enumerate() {
        let mut bytes = utf16(name);
        bytes.resize(68, 0);
        fonts.push(record(0, rt::FONT_ENTITY, i as u16, &bytes)?);
    }
    let drawings = number(w.drawing_counts.len())?;
    let mut dgg = words(&[(drawings + 1) * 1024, drawings + 1, w.drawing_counts.iter().sum(), drawings]);
    for (i, count) in w.drawing_counts.iter().enumerate() {
        dgg.extend(words(&[number(i + 1)?, *count]));
    }
    let dgg = container(
        rt::DRAWING_GROUP,
        0,
        &[container(rt::DGG_CONTAINER, 0, &[atom(0xf006, &dgg)?, container(rt::BSTORE, w.pictures.len() as u16, &w.pictures)?])?],
    )?;
    let mut doc_atom = words(&[(p.slide_size.width * 8.0).round() as u32, (p.slide_size.height * 8.0).round() as u32, 4320, 5760, 1, 1, 3, 0]);
    doc_atom.extend_from_slice(&(p.first_slide_number.min(u32::from(u16::MAX)) as u16).to_le_bytes());
    doc_atom.extend_from_slice(&[0; 6]);
    let environment = container(rt::ENVIRONMENT, 0, &[container(rt::FONT_COLLECTION, 0, &fonts)?, atom(0x0fa9, &[0; 4])?, default_style(4)?])?;
    let master_ref = atom(rt::SLIDE_PERSIST_ATOM, &words(&[2, 0, 0, MASTER, 0]))?;
    let document = container(
        rt::DOCUMENT,
        0,
        &[
            record(1, rt::DOCUMENT_ATOM, 0, &doc_atom)?,
            environment,
            dgg,
            container(rt::SLIDE_LIST_WITH_TEXT, 1, &[master_ref])?,
            container(rt::SLIDE_LIST_WITH_TEXT, 0, &slide_list)?,
            container(rt::SLIDE_LIST_WITH_TEXT, 2, &notes_list)?,
            atom(0x03ea, &[])?,
        ],
    )?;
    let mut stream = document;
    let mut offsets = vec![0];
    for page in pages {
        offsets.push(number(stream.len())?);
        if page.len() > MAX_BYTES.saturating_sub(stream.len()) {
            return Err(error("presentation exceeds 256 MiB"));
        }
        stream.extend(page);
    }
    let dir_at = number(stream.len())?;
    let mut directory = words(&[1 | (number(offsets.len())? << 20)]);
    directory.extend(words(&offsets));
    stream.extend(atom(rt::PERSIST_DIRECTORY, &directory)?);
    let edit_at = number(stream.len())?;
    let mut edit = words(&[255 + number(p.slides.len())?, 0x0300_1fe9, 0, dir_at, 1, number(offsets.len() + 1)?]);
    edit.extend_from_slice(&[1, 0, 0, 0]);
    stream.extend(atom(rt::USER_EDIT, &edit)?);
    let mut user = words(&[20, 0xe391_c05f, edit_at]);
    user.extend_from_slice(&[0, 0, 0xf4, 3, 3, 0, 0, 0]);
    user.extend_from_slice(&8u32.to_le_bytes());
    let user = atom(rt::CURRENT_USER, &user)?;
    let mut file = cfb::CompoundFile::create_with_version(cfb::Version::V3, Cursor::new(Vec::new())).map_err(|e| error(e.to_string()))?;
    if stream.len().saturating_add(w.picture_stream.len()) > MAX_BYTES {
        return Err(error("presentation streams exceed 256 MiB"));
    }
    for (name, bytes) in [("/PowerPoint Document", stream), ("/Current User", user), ("/Pictures", w.picture_stream)] {
        let mut output = file.create_stream(name).map_err(|e| error(e.to_string()))?;
        output.write_all(&bytes).map_err(|e| error(e.to_string()))?;
    }
    file.flush().map_err(|e| error(e.to_string()))?;
    Ok(file.into_inner().into_inner())
}
