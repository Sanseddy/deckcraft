#![allow(clippy::unwrap_used, clippy::expect_used)]

use deckcraft_geom::Xfrm;
use deckcraft_model::text::{Align, BodyProps, ParaProps, Paragraph, Run, RunProps, TextBody};
use deckcraft_model::{ColorRef, Fill, Geom, Line, Presentation, Rgba, Shape, ShapeId};
use std::sync::Arc;

fn text(value: &str) -> TextBody {
    TextBody {
        paragraphs: vec![Paragraph {
            runs: vec![Run::with(
                value,
                RunProps {
                    bold: Some(true),
                    italic: Some(true),
                    size: Some(24.0),
                    font: Some("Inter".into()),
                    fill: Some(Fill::solid(ColorRef::rgb(Rgba::rgb(10, 20, 30)))),
                    ..Default::default()
                },
            )],
            props: ParaProps { align: Some(Align::Center), ..Default::default() },
            ..Default::default()
        }],
        body: BodyProps::default(),
        ..Default::default()
    }
}

fn document() -> Presentation {
    let mut p = deckcraft_model::defaults::new_presentation(None);
    let slide = Arc::make_mut(p.slides.first_mut().unwrap());
    slide.shapes = vec![Shape {
        id: ShapeId(900),
        geom: Geom::preset("ellipse"),
        xfrm: Some(Xfrm::new(40.0, 50.0, 300.0, 180.0)),
        fill: Some(Fill::solid(ColorRef::rgb(Rgba::rgb(210, 40, 80)))),
        line: Some(Line::none()),
        text: Some(text("Привет 😀 — legacy PPT")),
        ..Default::default()
    }];
    slide.notes = text("Заметки докладчика");
    slide.hidden = true;
    let mut second = slide.clone();
    second.hidden = false;
    second.shapes.clear();
    p.slides.push(Arc::new(second));
    p
}

#[test]
fn writes_real_ppt_with_editable_unicode_text_shapes_notes_and_hidden_slides() {
    let p = document();
    assert!(!crate::needs_preview(&p, p.slides.first().unwrap()));
    let bytes = crate::export(&p, &[]).unwrap();
    assert!(bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0]));
    // An independent CFB implementation validates both small-stream and normal FAT storage.
    let mut compound = cfb::CompoundFile::open(std::io::Cursor::new(&bytes)).unwrap();
    assert!(compound.open_stream("/Current User").unwrap().len() >= 32);
    assert!(compound.open_stream("/PowerPoint Document").unwrap().len() > 1024);
    let back = crate::import(&bytes).unwrap();
    assert_eq!(back.slides.len(), 2);
    assert_eq!(back.slide_size, p.slide_size);
    let slide = back.slides.first().unwrap();
    assert!(slide.hidden);
    assert!(!back.slides.get(1).unwrap().hidden);
    assert_eq!(slide.notes.text(), "Заметки докладчика");
    let shape = slide.shapes.first().unwrap();
    assert_eq!(shape.geom.preset_name(), Some("ellipse"));
    assert_eq!(shape.xfrm.map(|x| (x.x, x.y, x.w, x.h)), Some((40.0, 50.0, 300.0, 180.0)));
    let body = shape.text.as_ref().unwrap();
    assert_eq!(body.text(), "Привет 😀 — legacy PPT");
    let para = body.paragraphs.first().unwrap();
    assert_eq!(para.props.align, Some(Align::Center));
    let props = &para.runs.first().unwrap().props;
    assert_eq!((props.bold, props.italic, props.size), (Some(true), Some(true), Some(24.0)));
    assert!(!crate::needs_preview(&back, slide), "basic imported objects stay editable on another save");
    let second_save = crate::import(&crate::export(&back, &[]).unwrap()).unwrap();
    assert_eq!(second_save.slides.first().unwrap().shapes.first().unwrap().text.as_ref().unwrap().text(), body.text());
    if let Some(dir) = std::env::var_os("DECKCRAFT_PPT_QA") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("editable-original.ppt"), bytes).unwrap();
    }
}

#[test]
fn unsupported_shapes_need_an_explicit_preview_and_previews_are_real_pictures() {
    let mut p = document();
    Arc::make_mut(p.slides.first_mut().unwrap()).shapes.first_mut().unwrap().geom = Geom::preset("unsupported-preset");
    assert!(crate::needs_preview(&p, p.slides.first().unwrap()));
    assert!(crate::export(&p, &[]).is_err());
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = png.into_inner();
    let bytes = crate::export(&p, &[Some(png.clone())]).unwrap();
    // Independent readers resolve FBSE offsets into a separate Pictures stream. An embedded
    // BLIP alone could round-trip through our importer but was invisible to Apache POI.
    let mut compound = cfb::CompoundFile::open(std::io::Cursor::new(&bytes)).unwrap();
    assert!(compound.open_stream("/Pictures").unwrap().len() > png.len() as u64);
    if let Some(dir) = std::env::var_os("DECKCRAFT_PPT_QA") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("preview-original.ppt"), &bytes).unwrap();
    }
    let back = crate::import(&bytes).unwrap();
    assert_eq!(back.slides.first().unwrap().shapes.len(), 1);
    assert_eq!(back.media.first().unwrap().data.as_slice(), png.as_slice());
}

#[test]
fn engine_save_and_export_select_ppt_from_the_suffix_and_report_compatibility() {
    let dir = std::env::temp_dir().join(format!("deckcraft-ppt-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("deck.ppt");
    let mut session = deckcraft_engine::Session::with_new();
    let saved = session.execute("file.saveAs", &serde_json::json!({"path": path.to_string_lossy()})).unwrap();
    assert!(saved.get("warning").and_then(serde_json::Value::as_str).is_some());
    let bytes = std::fs::read(&path).unwrap();
    assert!(crate::sniff(&bytes));
    assert_eq!(crate::import(&bytes).unwrap().slides.len(), 1);
    let exported = session.execute("file.export", &serde_json::json!({"path": dir.join("copy.PPT").to_string_lossy()})).unwrap();
    assert!(exported.get("warning").is_some());
    assert_eq!(deckcraft_engine::cmd::file::format_for_path("UPPER.PPT"), "ppt");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn invalid_sizes_and_excessive_counts_fail_without_panicking() {
    let mut p = document();
    for size in [f64::NAN, f64::INFINITY, -1.0, 1e100] {
        p.slide_size.width = size;
        assert!(crate::export(&p, &[]).is_err());
    }
    p.slide_size.width = 720.0;
    let first = p.slides.first().unwrap().clone();
    p.slides = vec![first; 1001];
    assert!(crate::export(&p, &[]).is_err());
}
