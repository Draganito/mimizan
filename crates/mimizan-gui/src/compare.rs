//! The compare window: one spot of the picture from Mimizan and from each
//! demosaicer, panned and zoomed together.
//!
//! Mimizan is a crop of the full separation. The four demosaicers run on a
//! tile of the mosaic around that spot, with a margin the algorithms need
//! and which is not shown. A small move stays inside the tile.

use crate::convert::separate_tile;
use crate::session::{develop_window, source_rect, DetailRect, Session, SharpenLoupe};
use mimizan_core::cfa::BayerPhase;
use mimizan_core::decode::SensorKind;
use mimizan_core::ingest::{self, WhiteBalance};
use mimizan_core::mix::Weights;
use mimizan_core::plane::Plane;
use mimizan_core::recipe::CropRect;
use mimizan_core::separate::{MaskMode, Separation};
use mimizan_core::tone::Tone;
use mimizan_demosaic::Method;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::session::Converter;

/// Pixels of mosaic kept around the shown spot and then discarded.
pub const MARGIN: usize = 32;
/// Extra tile beyond the margin, so a short drag does not demosaic again.
const SLACK: usize = 96;
/// Widest source window a panel may ask for.
pub const MAX_VIEW: usize = 1600;
/// Most screen pixels per picture pixel.
pub const MAX_ZOOM: f32 = 8.0;

/// Source pixels a panel shows at this zoom. One screen pixel per picture
/// pixel is zoom 1.
pub fn source_extent(cell_px: [usize; 2], zoom: f32) -> [usize; 2] {
    let z = zoom.max(1.0e-3);
    let side = |px: usize| (px as f32 / z).round() as usize;
    [side(cell_px[0]).clamp(1, MAX_VIEW), side(cell_px[1]).clamp(1, MAX_VIEW)]
}

/// The shown rectangle, in picture pixels, centred on `at` (0..1) and kept
/// inside the picture.
pub fn picture_window(pic_w: usize, pic_h: usize, at: [f64; 2], view_w: usize, view_h: usize) -> DetailRect {
    if pic_w == 0 || pic_h == 0 {
        return DetailRect { x: 0, y: 0, w: 1, h: 1 };
    }
    let w = view_w.clamp(1, pic_w);
    let h = view_h.clamp(1, pic_h);
    let x = (at[0] * pic_w as f64 - w as f64 * 0.5).round().clamp(0.0, (pic_w - w) as f64) as usize;
    let y = (at[1] * pic_h as f64 - h as f64 * 0.5).round().clamp(0.0, (pic_h - h) as f64) as usize;
    DetailRect { x, y, w, h }
}

/// Picture rectangle shifted onto the full upright frame when a crop is set.
pub fn to_upright(picture: DetailRect, crop: Option<CropRect>) -> DetailRect {
    let (ox, oy) = crop.map(|c| (c.x as usize, c.y as usize)).unwrap_or((0, 0));
    DetailRect {
        x: picture.x.saturating_add(ox),
        y: picture.y.saturating_add(oy),
        w: picture.w,
        h: picture.h,
    }
}

/// Even tile that contains `visible` plus the margin and some slack, inside
/// `bounds`. `None` when the frame is below the demosaicer's 64-pixel minimum.
pub fn sensor_tile(bounds: [usize; 2], visible: DetailRect) -> Option<DetailRect> {
    let bw = bounds[0] & !1;
    let bh = bounds[1] & !1;
    if bw < 64 || bh < 64 || visible.w == 0 || visible.h == 0 {
        return None;
    }
    let pad = MARGIN + SLACK;
    let vx1 = (visible.x + visible.w).min(bw);
    let vy1 = (visible.y + visible.h).min(bh);
    if vx1 <= visible.x || vy1 <= visible.y {
        return None;
    }
    let mut x0 = visible.x.saturating_sub(pad) & !1;
    let mut y0 = visible.y.saturating_sub(pad) & !1;
    let mut x1 = ((visible.x + visible.w + pad).min(bw) + 1) & !1;
    let mut y1 = ((visible.y + visible.h + pad).min(bh) + 1) & !1;
    x1 = x1.min(bw);
    y1 = y1.min(bh);
    if x0 > visible.x {
        x0 = visible.x & !1;
    }
    if y0 > visible.y {
        y0 = visible.y & !1;
    }
    if x1 < vx1 {
        x1 = ((vx1 + 1) & !1).min(bw);
        if x1 < vx1 {
            return None;
        }
    }
    if y1 < vy1 {
        y1 = ((vy1 + 1) & !1).min(bh);
        if y1 < vy1 {
            return None;
        }
    }
    grow_even(&mut x0, &mut x1, bw, 64);
    grow_even(&mut y0, &mut y1, bh, 64);
    if x1 <= x0 || y1 <= y0 || x1 - x0 < 64 || y1 - y0 < 64 {
        return None;
    }
    if x0 > visible.x || y0 > visible.y || x1 < vx1 || y1 < vy1 {
        return None;
    }
    Some(DetailRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// Grow an even half-open span to `min` without passing `bound`.
fn grow_even(a: &mut usize, b: &mut usize, bound: usize, min: usize) {
    if b.saturating_sub(*a) >= min {
        return;
    }
    let grow = min - (b.saturating_sub(*a));
    let shift = (*a).min(grow);
    *a -= shift;
    if b.saturating_sub(*a) < min {
        *b = (*a + min).min(bound);
    }
}

/// The cached tile still holds a margin around `visible`, or the frame edge
/// where there is no further mosaic.
pub fn tile_covers(tile: DetailRect, visible: DetailRect, bounds: [usize; 2]) -> bool {
    let need_x0 = visible.x.saturating_sub(MARGIN);
    let need_y0 = visible.y.saturating_sub(MARGIN);
    let need_x1 = (visible.x + visible.w + MARGIN).min(bounds[0]);
    let need_y1 = (visible.y + visible.h + MARGIN).min(bounds[1]);
    tile.x <= need_x0 && tile.y <= need_y0 && tile.x + tile.w >= need_x1 && tile.y + tile.h >= need_y1
}

#[derive(Clone, PartialEq)]
pub struct CompareKey {
    pub frame: usize,
    pub weights: Weights,
    pub place_bits: u64,
    pub tone: Tone,
    pub orientation: u16,
    pub crop: Option<CropRect>,
    pub at: [u64; 2],
    pub view: [usize; 2],
}

pub struct CompareRequest {
    pub session: Arc<Session>,
    pub weights: Weights,
    pub place: f64,
    pub tone: Tone,
    pub orientation: u16,
    /// Window on the export-upright frame. `None` is the whole negative.
    pub crop: Option<CropRect>,
    pub at: [f64; 2],
    pub view: [usize; 2],
    pub generation: u64,
}

pub struct TileResult {
    pub generation: u64,
    pub index: usize,
    pub image: SharpenLoupe,
}

enum Msg {
    Render(CompareRequest),
    Clear,
}

struct MosaicHold {
    frame: usize,
    wb: WhiteBalance,
    plane: Plane,
    phase: BayerPhase,
}

struct MimizanHold {
    frame: usize,
    wb: WhiteBalance,
    mask: MaskMode,
    fix_defects: bool,
    reconstruct: bool,
    sep: Separation,
}

struct DemoHold {
    frame: usize,
    wb: WhiteBalance,
    fix_defects: bool,
    method: Method,
    sensor: DetailRect,
    sep: Separation,
}

struct Cache {
    mosaic: Option<MosaicHold>,
    mimizan: Option<MimizanHold>,
    tiles: [Option<DemoHold>; 4],
}

impl Cache {
    fn clear(&mut self) {
        *self = Self { mosaic: None, mimizan: None, tiles: [None, None, None, None] };
    }
}

pub struct Compare {
    tx: Sender<Msg>,
    rx: Receiver<TileResult>,
}

impl Compare {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, reqs) = channel::<Msg>();
        let (results, rx) = channel::<TileResult>();
        thread::Builder::new()
            .name("mimizan-compare".into())
            .spawn(move || {
                let mut cache = Cache { mosaic: None, mimizan: None, tiles: [None, None, None, None] };
                while let Ok(msg) = reqs.recv() {
                    match msg {
                        Msg::Clear => cache.clear(),
                        Msg::Render(req) => {
                            if let Step::Stop = produce(req, &mut cache, &results, &reqs, &ctx) {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("spawn compare worker");
        Self { tx, rx }
    }

    pub fn request(&self, r: CompareRequest) {
        let _ = self.tx.send(Msg::Render(r));
    }

    pub fn clear(&self) {
        let _ = self.tx.send(Msg::Clear);
    }

    pub fn poll(&self) -> Vec<TileResult> {
        let mut all = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            all.push(r);
        }
        all
    }
}

enum Step {
    Next,
    Stop,
}

/// Render the five tiles. A newer request replaces this one. `Stop` means
/// the window side of the channel has gone.
fn produce(
    mut req: CompareRequest,
    cache: &mut Cache,
    tx: &Sender<TileResult>,
    rx: &Receiver<Msg>,
    ctx: &egui::Context,
) -> Step {
    loop {
        match drain(rx, cache) {
            Drain::Newer(r) => req = r,
            Drain::Clear => return Step::Next,
            Drain::Stop => return Step::Stop,
            Drain::Same => {}
        }
        let mut superseded = None;
        for index in 0..5 {
            match drain(rx, cache) {
                Drain::Newer(r) => {
                    superseded = Some(r);
                    break;
                }
                Drain::Clear => return Step::Next,
                Drain::Stop => return Step::Stop,
                Drain::Same => {}
            }
            let generation = req.generation;
            let rendered =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_index(&req, cache, index)));
            let image = match rendered {
                Ok(image) => image,
                Err(payload) => {
                    tracing::error!("compare tile {index} failed: {}", panic_message(&payload));
                    SharpenLoupe { gray: Vec::new(), width: 0, height: 0 }
                }
            };
            if tx.send(TileResult { generation, index, image }).is_err() {
                return Step::Stop;
            }
            ctx.request_repaint();
        }
        if let Some(r) = superseded {
            req = r;
            continue;
        }
        return Step::Next;
    }
}

enum Drain {
    Same,
    Newer(CompareRequest),
    Clear,
    Stop,
}

fn drain(rx: &Receiver<Msg>, cache: &mut Cache) -> Drain {
    let mut newer = None;
    let mut cleared = false;
    loop {
        match rx.try_recv() {
            Ok(Msg::Render(r)) => newer = Some(r),
            Ok(Msg::Clear) => {
                cache.clear();
                cleared = true;
                newer = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return Drain::Stop,
        }
    }
    if let Some(r) = newer {
        Drain::Newer(r)
    } else if cleared {
        Drain::Clear
    } else {
        Drain::Same
    }
}

fn render_index(req: &CompareRequest, cache: &mut Cache, index: usize) -> SharpenLoupe {
    let empty = SharpenLoupe { gray: Vec::new(), width: 0, height: 0 };
    let Some(sensor) = sensor_visible(req) else { return empty };
    if index == 0 {
        return mimizan_gray(req, cache, sensor).unwrap_or_else(|e| {
            tracing::error!("compare Mimizan: {e}");
            empty
        });
    }
    let method = match index {
        1 => Method::Amaze,
        2 => Method::Rcd,
        3 => Method::Dcb,
        _ => Method::Lmmse,
    };
    demosaic_gray(req, cache, method, sensor).unwrap_or_else(|e| {
        tracing::error!("compare {}: {e}", method.label());
        empty
    })
}

fn sensor_visible(req: &CompareRequest) -> Option<DetailRect> {
    let sep = req.session.separation();
    let (sw, sh) = (sep.lum.width, sep.lum.height);
    let (uw, uh) = if matches!(req.orientation, 5..=8) { (sh, sw) } else { (sw, sh) };
    let (pic_w, pic_h) = match req.crop {
        Some(c) => (c.w as usize, c.h as usize),
        None => (uw, uh),
    };
    let picture = picture_window(pic_w, pic_h, req.at, req.view[0], req.view[1]);
    let upright = to_upright(picture, req.crop);
    if upright.x >= uw || upright.y >= uh {
        return None;
    }
    let upright = DetailRect {
        x: upright.x,
        y: upright.y,
        w: upright.w.min(uw - upright.x).max(1),
        h: upright.h.min(uh - upright.y).max(1),
    };
    Some(source_rect(req.orientation, sw, sh, upright))
}

fn mimizan_gray(req: &CompareRequest, cache: &mut Cache, sensor: DetailRect) -> Result<SharpenLoupe, String> {
    if req.session.develop.converter != Converter::Mimizan {
        ensure_mimizan(req, cache)?;
    }
    let sep = if req.session.develop.converter == Converter::Mimizan {
        req.session.separation()
    } else {
        &cache.mimizan.as_ref().ok_or("missing separation")?.sep
    };
    develop_window(sep, sensor, req.orientation, &req.weights, req.place, &req.tone)
        .ok_or_else(|| "empty window".into())
}

fn ensure_mimizan(req: &CompareRequest, cache: &mut Cache) -> Result<(), String> {
    let frame = frame_id(req);
    let d = &req.session.develop;
    if let Some(h) = &cache.mimizan {
        if h.frame == frame
            && h.wb == d.wb
            && h.mask == d.mask
            && h.fix_defects == d.fix_defects
            && h.reconstruct == d.reconstruct
        {
            return Ok(());
        }
    }
    let params = d.negative_params(req.session.camera_file.clone());
    let neg = crate::convert::develop(&req.session.frame, &params, 0, Converter::Mimizan)
        .map_err(|e| e.to_string())?;
    let sep = neg.separation.ok_or("separation was not kept")?;
    cache.mimizan = Some(MimizanHold {
        frame,
        wb: d.wb,
        mask: d.mask,
        fix_defects: d.fix_defects,
        reconstruct: d.reconstruct,
        sep,
    });
    Ok(())
}

fn demosaic_gray(
    req: &CompareRequest,
    cache: &mut Cache,
    method: Method,
    sensor: DetailRect,
) -> Result<SharpenLoupe, String> {
    let sep = req.session.separation();
    let bounds = [sep.lum.width, sep.lum.height];
    if let Some(h) = cache.tiles.iter().flatten().find(|h| {
        h.frame == frame_id(req)
            && h.wb == req.session.develop.wb
            && h.fix_defects == req.session.develop.fix_defects
            && h.method == method
            && tile_covers(h.sensor, sensor, bounds)
    }) {
        let local =
            DetailRect { x: sensor.x - h.sensor.x, y: sensor.y - h.sensor.y, w: sensor.w, h: sensor.h };
        return develop_window(&h.sep, local, req.orientation, &req.weights, req.place, &req.tone)
            .ok_or_else(|| "empty window".into());
    }
    let tile = sensor_tile(bounds, sensor).ok_or("the frame is too small to demosaic")?;
    let (samples, phase) = tile_samples(req, cache, tile)?;
    let sep = separate_tile(method, tile.w, tile.h, &samples, phase)?;
    let local = DetailRect { x: sensor.x - tile.x, y: sensor.y - tile.y, w: sensor.w, h: sensor.h };
    let gray = develop_window(&sep, local, req.orientation, &req.weights, req.place, &req.tone)
        .ok_or("empty window")?;
    let slot = match method {
        Method::Amaze => 0,
        Method::Rcd => 1,
        Method::Dcb => 2,
        Method::Lmmse => 3,
    };
    cache.tiles[slot] = Some(DemoHold {
        frame: frame_id(req),
        wb: req.session.develop.wb,
        fix_defects: req.session.develop.fix_defects,
        method,
        sensor: tile,
        sep,
    });
    Ok(gray)
}

fn tile_samples(
    req: &CompareRequest,
    cache: &mut Cache,
    tile: DetailRect,
) -> Result<(Vec<f64>, BayerPhase), String> {
    if req.session.develop.fix_defects {
        ensure_mosaic(req, cache)?;
        let hold = cache.mosaic.as_ref().ok_or("missing mosaic")?;
        return Ok((crop_samples(&hold.plane, tile), hold.phase));
    }
    let phase = active_phase(&req.session).ok_or("no colour mosaic")?;
    Ok((raw_tile(&req.session, tile, phase), phase))
}

fn ensure_mosaic(req: &CompareRequest, cache: &mut Cache) -> Result<(), String> {
    let frame = frame_id(req);
    let wb = req.session.develop.wb;
    if let Some(h) = &cache.mosaic {
        if h.frame == frame && h.wb == wb {
            return Ok(());
        }
    }
    let params = req.session.develop.negative_params(req.session.camera_file.clone());
    let mosaic = ingest::ingest(&req.session.frame, &params.ingest).map_err(|e| e.to_string())?;
    let phase = mosaic.phase.ok_or("no colour mosaic")?;
    cache.mosaic = Some(MosaicHold { frame, wb, plane: mosaic.plane, phase });
    Ok(())
}

fn active_phase(session: &Session) -> Option<BayerPhase> {
    let crop = ingest::even_crop(session.frame.crop);
    match session.frame.kind {
        SensorKind::Bayer(ph) => Some(ph.shifted(crop.x, crop.y)),
        SensorKind::Mono => None,
    }
}

/// Balanced mosaic of an even tile, using the balance of the open file.
fn raw_tile(session: &Session, tile: DetailRect, phase: BayerPhase) -> Vec<f64> {
    let frame = &session.frame;
    let active = ingest::even_crop(frame.crop);
    let wb = session.wb();
    let mut out = vec![0.0; tile.w * tile.h];
    for y in 0..tile.h {
        let ay = active.y + tile.y + y;
        for x in 0..tile.w {
            let ax = active.x + tile.x + x;
            let raw = frame.data.get(ay * frame.width + ax);
            let black = frame.black_at(ax, ay);
            let white = frame.white_at(ax, ay);
            let v = (raw - black) / (white - black);
            out[y * tile.w + x] = v * wb[phase.color_at(x, y) as usize];
        }
    }
    out
}

fn crop_samples(plane: &Plane, tile: DetailRect) -> Vec<f64> {
    let mut out = vec![0.0; tile.w * tile.h];
    for y in 0..tile.h {
        let row = plane.row(tile.y + y);
        out[y * tile.w..(y + 1) * tile.w].copy_from_slice(&row[tile.x..tile.x + tile.w]);
    }
    out
}

fn frame_id(req: &CompareRequest) -> usize {
    Arc::as_ptr(&req.session.frame) as usize
}

/// What the compare window may zoom to. 1:1 stays inside the range: the floor
/// is the wider view, and it never rises above one.
pub fn zoom_limits(cell_px: [usize; 2]) -> (f32, f32) {
    let widest = (cell_px[0].max(cell_px[1]) as f32 / MAX_VIEW as f32).max(1.0 / 16.0);
    (widest.min(1.0), MAX_ZOOM)
}

/// State the main window and the compare window share.
pub struct ComparePane {
    pub state: Mutex<CompareState>,
}

pub struct CompareState {
    pub tex: [Option<egui::TextureHandle>; 5],
    pub failed: [bool; 5],
    pub updating: bool,
    pub active: usize,
    pub pic: [usize; 2],
    pub zoom: f32,
    pub at: [f64; 2],
    pub view: [usize; 2],
    /// The window changed the point or the zoom. The main window copies them back.
    pub from_child: bool,
    pub close: bool,
}

impl Default for ComparePane {
    fn default() -> Self {
        Self {
            state: Mutex::new(CompareState {
                tex: [None, None, None, None, None],
                failed: [false; 5],
                updating: false,
                active: 0,
                pic: [0, 0],
                zoom: 1.0,
                at: [0.5, 0.5],
                view: [0, 0],
                from_child: false,
                close: false,
            }),
        }
    }
}

pub fn lock(pane: &ComparePane) -> std::sync::MutexGuard<'_, CompareState> {
    pane.state.lock().unwrap_or_else(|e| e.into_inner())
}

/// One pass of the compare window. Drag moves the shared point, the wheel
/// zooms every panel, a double-click returns to one picture pixel per
/// screen pixel.
pub fn compare_window(ui: &mut egui::Ui, pane: &ComparePane) {
    let snap = {
        let g = lock(pane);
        Draw {
            tex: g.tex.clone(),
            failed: g.failed,
            updating: g.updating,
            active: g.active,
            pic: g.pic,
            zoom: g.zoom,
            at: g.at,
        }
    };
    let Some(frame) = layout(ui, &snap) else { return };
    let mut g = lock(pane);
    g.view = frame.view;
    if frame.reset {
        g.zoom = 1.0;
        g.from_child = true;
    } else if frame.zoom != snap.zoom {
        g.zoom = frame.zoom;
        g.from_child = true;
    }
    if frame.drag != egui::Vec2::ZERO {
        g.at = nudge_at(snap.at, frame.drag, frame.ppp, g.zoom, snap.pic);
        g.from_child = true;
    }
    let (lo, hi) = zoom_limits(frame.cell_px);
    g.zoom = g.zoom.clamp(lo, hi);
}

struct Draw {
    tex: [Option<egui::TextureHandle>; 5],
    failed: [bool; 5],
    updating: bool,
    active: usize,
    pic: [usize; 2],
    zoom: f32,
    at: [f64; 2],
}

struct Frame {
    view: [usize; 2],
    cell_px: [usize; 2],
    zoom: f32,
    drag: egui::Vec2,
    reset: bool,
    ppp: f32,
}

fn layout(ui: &mut egui::Ui, snap: &Draw) -> Option<Frame> {
    let avail = ui.available_rect_before_wrap();
    let gap = 8.0;
    let cell_w = (avail.width() - gap * 2.0) / 3.0;
    let cell_h = (avail.height() - gap) / 2.0;
    if cell_w < 40.0 || cell_h < 40.0 {
        return None;
    }
    let ppp = ui.pixels_per_point().max(f32::MIN_POSITIVE);
    let label_h = 22.0;
    let img_w = (cell_w - 12.0).max(8.0);
    let img_h = (cell_h - label_h - 12.0).max(8.0);
    let cell_px = [(img_w * ppp).round() as usize, (img_h * ppp).round() as usize];
    let (lo, hi) = zoom_limits(cell_px);
    let mut zoom = snap.zoom;
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    let pinch = ui.input(|i| i.zoom_delta());
    if scroll != 0.0 {
        zoom *= 1.15_f32.powf(scroll / 40.0);
    }
    if (pinch - 1.0).abs() > 1.0e-3 {
        zoom *= pinch;
    }
    zoom = zoom.clamp(lo, hi);
    let view = source_extent(cell_px, zoom);

    let mut drag = egui::Vec2::ZERO;
    let mut reset = false;
    for i in 0..6 {
        let col = (i % 3) as f32;
        let row = (i / 3) as f32;
        let rect = egui::Rect::from_min_size(
            avail.left_top() + egui::Vec2::new(col * (cell_w + gap), row * (cell_h + gap)),
            egui::Vec2::new(cell_w, cell_h),
        );
        if i == 5 {
            paint_note(ui, rect, zoom, snap.updating);
        } else if let Some(act) = paint_cell(ui, rect, i, label_h, snap, zoom) {
            match act {
                Act::Drag(d) => drag += d,
                Act::Reset => reset = true,
            }
        }
    }
    Some(Frame { view, cell_px, zoom, drag, reset, ppp })
}

enum Act {
    Drag(egui::Vec2),
    Reset,
}

fn paint_cell(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    index: usize,
    label_h: f32,
    snap: &Draw,
    zoom: f32,
) -> Option<Act> {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(22));
    let name = Converter::ALL[index].label();
    let color = if snap.active == index {
        egui::Color32::from_rgb(255, 210, 60)
    } else {
        egui::Color32::from_gray(220)
    };
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.top() + 4.0),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::proportional(14.0),
        color,
    );
    let img = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 6.0, rect.top() + label_h),
        egui::pos2(rect.right() - 6.0, rect.bottom() - 6.0),
    );
    let response = ui
        .allocate_rect(img, egui::Sense::click_and_drag())
        .on_hover_text("Drag moves every panel. Double-click returns to 1:1.");
    if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if let Some(tex) = &snap.tex[index] {
        let ppp = ui.pixels_per_point().max(f32::MIN_POSITIVE);
        let [w, h] = tex.size();
        let disp = egui::vec2(w as f32 / ppp * zoom, h as f32 / ppp * zoom);
        let drawn = egui::Rect::from_center_size(
            img.center(),
            egui::vec2(disp.x.min(img.width()), disp.y.min(img.height())),
        );
        painter.image(
            tex.id(),
            drawn,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        let msg = if snap.failed[index] { "failed" } else { "rendering…" };
        painter.text(
            img.center(),
            egui::Align2::CENTER_CENTER,
            msg,
            egui::FontId::proportional(13.0),
            egui::Color32::from_gray(140),
        );
    }
    if response.double_clicked() {
        Some(Act::Reset)
    } else if response.dragged() {
        Some(Act::Drag(response.drag_delta()))
    } else {
        None
    }
}

fn paint_note(ui: &mut egui::Ui, rect: egui::Rect, zoom: f32, updating: bool) {
    let painter = ui.painter_at(rect);
    let color = egui::Color32::from_gray(170);
    let font = egui::FontId::proportional(13.0);
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y - 36.0),
        egui::Align2::LEFT_CENTER,
        format!("{zoom:.2}×"),
        egui::FontId::proportional(22.0),
        egui::Color32::from_gray(230),
    );
    let line = if updating {
        "updating"
    } else if (zoom - 1.0).abs() < 0.02 {
        "one picture pixel per screen pixel"
    } else {
        "every panel shows this scale"
    };
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        line,
        font.clone(),
        color,
    );
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y + 14.0),
        egui::Align2::LEFT_CENTER,
        "drag moves every panel",
        font.clone(),
        color,
    );
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y + 32.0),
        egui::Align2::LEFT_CENTER,
        "scroll zooms · double-click is 1:1",
        font,
        color,
    );
}

fn nudge_at(at: [f64; 2], delta: egui::Vec2, ppp: f32, zoom: f32, pic: [usize; 2]) -> [f64; 2] {
    let z = f64::from(zoom.max(1.0e-3));
    let iw = (pic[0].saturating_sub(1) as f64).max(1.0);
    let ih = (pic[1].saturating_sub(1) as f64).max(1.0);
    [
        (at[0] - f64::from(delta.x * ppp) / z / iw).clamp(0.0, 1.0),
        (at[1] - f64::from(delta.y * ppp) / z / ih).clamp(0.0, 1.0),
    ]
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_centred_and_stays_inside() {
        let r = picture_window(1000, 800, [0.5, 0.5], 100, 80);
        assert_eq!((r.x, r.y, r.w, r.h), (450, 360, 100, 80));
        let corner = picture_window(1000, 800, [0.0, 0.0], 100, 80);
        assert_eq!((corner.x, corner.y), (0, 0));
        let big = picture_window(40, 30, [0.5, 0.5], 100, 80);
        assert_eq!((big.w, big.h), (40, 30));
    }

    #[test]
    fn the_tile_contains_the_spot_and_a_nudge_stays_inside() {
        let bounds = [2000, 1500];
        let visible = DetailRect { x: 400, y: 300, w: 200, h: 120 };
        let tile = sensor_tile(bounds, visible).unwrap();
        assert_eq!(tile.x % 2, 0);
        assert_eq!(tile.y % 2, 0);
        assert_eq!(tile.w % 2, 0);
        assert_eq!(tile.h % 2, 0);
        assert!(tile_covers(tile, visible, bounds));
        let nudged = DetailRect { x: visible.x + 1, y: visible.y + 1, ..visible };
        assert!(tile_covers(tile, nudged, bounds));
        let left = DetailRect { x: 10, y: 10, w: 80, h: 70 };
        let edge = sensor_tile(bounds, left).unwrap();
        assert!(edge.x <= left.x && edge.y <= left.y);
        assert!(edge.x + edge.w >= left.x + left.w);
        assert!(edge.y + edge.h >= left.y + left.h);
        assert!(sensor_tile([40, 40], DetailRect { x: 0, y: 0, w: 40, h: 40 }).is_none());
    }

    #[test]
    fn one_to_one_stays_inside_the_zoom_range() {
        let (lo, hi) = zoom_limits([400, 300]);
        assert!(lo < 1.0 && hi > 1.0);
        let (lo, _) = zoom_limits([2000, 1800]);
        assert!((lo - 1.0).abs() < 1.0e-6);
    }
}
