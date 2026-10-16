//! tahoe-view: muestra en una ventana de Linux la pantalla del macOS que corre en tahoe-run.
//!
//! WindowServer dibuja en la memoria de vídeo emulada (`/dev/shm/tahoe-fb-<id>`, ver `tahoe-run/emu_iokit.inc`). En la
//! última página del archivo va una cabecera "TFB1" con ancho, alto, bytes por fila y formato (0 = B, G, R, X por píxel,
//! que es justo el formato de softbuffer: un u32 0RGB en little endian). El cursor, por software, está en
//! `/dev/shm/tahoe-fbcursor-<id>` (StdFBShmem_t): aquí solo se marca su posición (`cursorLoc`, +28).
//!
//! El teclado y el ratón de la ventana van a macOS (ver `entrada.rs`).
//!
//! Uso: `tahoe-view [archivo]` (por omisión, el `/dev/shm/tahoe-fb-*` más reciente; espera a que aparezca). F12 cierra.

mod entrada;

use std::fs::File;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use memmap2::Mmap;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey, PhysicalKey};

use entrada::Entrada;
use winit::window::{Window, WindowId};

const HDR_PAGE: usize = 4096;
const FRAME: Duration = Duration::from_millis(16); // ~60 Hz

/// Cabecera de la memoria de vídeo (struct fb_hdr de emu_iokit.inc).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Header {
    width: u32,
    height: u32,
    bytes_per_row: u32,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Valida la cabecera (`page`: la última página del archivo) para una memoria de vídeo de `data_len` bytes (lo que
/// precede a esa página); None si el archivo aún no está listo o no es una memoria de vídeo de tahoe-run.
fn parse_header(page: &[u8], data_len: usize) -> Option<Header> {
    if page.len() < 20 || &page[0..4] != b"TFB1" || u32_at(page, 16) != 0 {
        return None;
    }
    let hdr = Header { width: u32_at(page, 4), height: u32_at(page, 8), bytes_per_row: u32_at(page, 12) };
    let need = (hdr.bytes_per_row as usize).checked_mul(hdr.height as usize)?;
    let ok = hdr.width > 0 && hdr.height > 0 && hdr.bytes_per_row as u64 >= hdr.width as u64 * 4 && need <= data_len;
    ok.then_some(hdr)
}

/// La memoria de vídeo más reciente de /dev/shm.
fn newest_framebuffer() -> Option<PathBuf> {
    std::fs::read_dir("/dev/shm")
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("tahoe-fb-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// El cursor compartido del mismo arranque: tahoe-fb-<id> -> tahoe-fbcursor-<id>.
fn cursor_path(fb: &Path) -> Option<PathBuf> {
    let name = fb.file_name()?.to_str()?;
    Some(fb.with_file_name(format!("tahoe-fbcursor-{}", name.strip_prefix("tahoe-fb-")?)))
}

/// Proyección de solo lectura de un archivo de /dev/shm que otro proceso (WindowServer) escribe mientras tanto.
/// El contenido cambia por debajo, así que nunca se presta como `&[u8]` de larga vida: se copia con lecturas volátiles.
struct Shared {
    map: Mmap,
}

impl Shared {
    fn open(path: &Path) -> Option<Shared> {
        let file = File::open(path).ok()?;
        // SAFETY: la proyección es de solo lectura y solo se lee con copias (read_volatile); que otro proceso la
        // modifique a la vez puede dar un fotograma a medias, nunca un acceso fuera de la proyección.
        let map = unsafe { Mmap::map(&file) }.ok()?;
        Some(Shared { map })
    }
    fn len(&self) -> usize {
        self.map.len()
    }
    /// Copia `dst.len()` píxeles (u32) desde el desplazamiento `off`, comprobando los límites.
    fn copy_u32(&self, off: usize, dst: &mut [u32]) {
        let bytes = dst.len() * 4;
        assert!(off.checked_add(bytes).is_some_and(|end| end <= self.map.len()));
        let src = self.map.as_ptr();
        for (i, px) in dst.iter_mut().enumerate() {
            // SAFETY: off + 4 * i + 4 <= len, comprobado arriba; lectura volátil de memoria compartida.
            let raw = unsafe { std::ptr::read_volatile(src.add(off + 4 * i).cast::<[u8; 4]>()) };
            *px = u32::from_le_bytes(raw) & 0x00ff_ffff;
        }
    }
    fn i16_at(&self, off: usize) -> Option<i16> {
        if off + 2 > self.map.len() {
            return None;
        }
        // SAFETY: límites comprobados justo arriba.
        let raw = unsafe { std::ptr::read_volatile(self.map.as_ptr().add(off).cast::<[u8; 2]>()) };
        Some(i16::from_le_bytes(raw))
    }
    fn snapshot_header(&self) -> Option<Header> {
        let data_len = self.len().checked_sub(HDR_PAGE).filter(|&n| n > 0)?;
        let page: Vec<u8> = (0..HDR_PAGE)
            // SAFETY: data_len + i < len; lectura volátil de memoria compartida.
            .map(|i| unsafe { std::ptr::read_volatile(self.map.as_ptr().add(data_len + i)) })
            .collect();
        parse_header(&page, data_len)
    }
}

struct Screen {
    fb: Shared,
    cursor: Option<Shared>,
    hdr: Header,
}

impl Screen {
    fn open(path: &Path) -> Option<Screen> {
        let fb = Shared::open(path)?;
        let hdr = fb.snapshot_header()?;
        let cursor = cursor_path(path).and_then(|p| Shared::open(&p));
        eprintln!("tahoe-view: {}: {}x{}", path.display(), hdr.width, hdr.height);
        Some(Screen { fb, cursor, hdr })
    }
    /// Colocación de la pantalla en una ventana de `w`×`h`: escala entera hacia abajo si la ventana es más pequeña
    /// (si no, 1:1), centrada. Devuelve (escala, x, y, ancho, alto) de la imagen en la ventana.
    fn layout(&self, w: usize, h: usize) -> (usize, usize, usize, usize, usize) {
        let (sw, sh) = (self.hdr.width as usize, self.hdr.height as usize);
        let scale = (sw.div_ceil(w.max(1))).max(sh.div_ceil(h.max(1))).max(1);
        let (dw, dh) = (sw / scale, sh / scale);
        (scale, (w - dw.min(w)) / 2, (h - dh.min(h)) / 2, dw, dh)
    }
    /// Punto de la ventana -> píxel de la pantalla de macOS (dentro de sus límites).
    fn to_screen(&self, x: f64, y: f64, w: usize, h: usize) -> (i32, i32) {
        let (scale, ox, oy, _, _) = self.layout(w, h);
        let sx = ((x - ox as f64) * scale as f64).clamp(0.0, self.hdr.width as f64 - 1.0);
        let sy = ((y - oy as f64) * scale as f64).clamp(0.0, self.hdr.height as f64 - 1.0);
        (sx as i32, sy as i32)
    }
    /// Dibuja la pantalla de macOS (escalada al tamaño de la ventana, sin deformar) en `out` (ancho `w`).
    fn draw(&self, out: &mut [u32], w: usize, h: usize) {
        out.fill(0);
        let sw = self.hdr.width as usize;
        let (scale, ox, oy, dw, dh) = self.layout(w, h);
        let mut row = vec![0u32; sw];
        for y in 0..dh.min(h) {
            self.fb.copy_u32(y * scale * self.hdr.bytes_per_row as usize, &mut row);
            let dst = &mut out[(oy + y) * w + ox..(oy + y) * w + ox + dw.min(w - ox)];
            for (x, px) in dst.iter_mut().enumerate() {
                *px = row[x * scale];
            }
        }
        // posición del cursor (StdFBShmem_t.cursorLoc): una cruz pequeña
        if let Some(c) = &self.cursor {
            if let (Some(cx), Some(cy)) = (c.i16_at(28), c.i16_at(30)) {
                let (cx, cy) = (cx as isize / scale as isize + ox as isize, cy as isize / scale as isize + oy as isize);
                for d in -3isize..=3 {
                    for (x, y) in [(cx + d, cy), (cx, cy + d)] {
                        if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                            out[y as usize * w + x as usize] = 0x00ff_ffff;
                        }
                    }
                }
            }
        }
    }
}

struct App {
    path: Option<PathBuf>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    screen: Option<Screen>,
    next: Instant,
    entrada: Option<Entrada>,
    pos: (i32, i32),
    botones: i32,
}

impl App {
    fn try_open(&mut self) {
        if self.screen.is_some() {
            return;
        }
        let path = self.path.clone().or_else(newest_framebuffer);
        if let Some(p) = path {
            self.screen = Screen::open(&p);
            if self.screen.is_some() {
                self.entrada = entrada::ruta_entrada(&p).map(Entrada::new);
            }
            if let (Some(s), Some(w)) = (&self.screen, &self.window) {
                let _ = w.request_inner_size(PhysicalSize::new(s.hdr.width, s.hdr.height));
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("macOS (tahoe-run)").with_inner_size(PhysicalSize::new(1280, 800));
        let window = Rc::new(el.create_window(attrs).expect("no se pudo crear la ventana"));
        let ctx = softbuffer::Context::new(window.clone()).expect("softbuffer: contexto");
        self.surface = Some(softbuffer::Surface::new(&ctx, window.clone()).expect("softbuffer: superficie"));
        self.window = Some(window);
        self.try_open();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested
            | WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::F12), state: ElementState::Pressed, .. },
                ..
            } => el.exit(),
            WindowEvent::RedrawRequested => {
                let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else { return };
                let size = window.inner_size();
                let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
                if surface.resize(w, h).is_err() {
                    return;
                }
                let Ok(mut buf) = surface.buffer_mut() else { return };
                match &self.screen {
                    Some(s) => s.draw(&mut buf, w.get() as usize, h.get() as usize),
                    None => buf.fill(0),
                }
                let _ = buf.present();
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(k), state, repeat: false, .. }, .. } => {
                // sin las repeticiones de Linux: macOS repite por su cuenta mientras la tecla siga pulsada
                if let (Some(e), Some(u)) = (&self.entrada, entrada::uso_hid(k)) {
                    e.enviar(entrada::TECLA, 7, u as i32, (state == ElementState::Pressed) as i32);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (Some(s), Some(win)) = (&self.screen, &self.window) else { return };
                let size = win.inner_size();
                self.pos = s.to_screen(position.x, position.y, size.width as usize, size.height as usize);
                if let Some(e) = &self.entrada {
                    e.enviar(entrada::PUNTERO, self.pos.0, self.pos.1, self.botones);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 4,
                    _ => return,
                };
                if state == ElementState::Pressed { self.botones |= bit } else { self.botones &= !bit }
                if let Some(e) = &self.entrada {
                    e.enviar(entrada::PUNTERO, self.pos.0, self.pos.1, self.botones);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x.round() as i32, y.round() as i32),
                    MouseScrollDelta::PixelDelta(p) => ((p.x / 20.0).round() as i32, (p.y / 20.0).round() as i32),
                };
                if let (Some(e), true) = (&self.entrada, dx != 0 || dy != 0) {
                    e.enviar(entrada::RUEDA, dx, dy, 0);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next {
            self.try_open(); // hasta que WindowServer cree la memoria de vídeo
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.next = now + if self.screen.is_some() { FRAME } else { Duration::from_millis(500) };
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next));
    }
}

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let el = EventLoop::new().expect("no hay servidor gráfico (Wayland o X11)");
    let mut app = App { path, window: None, surface: None, screen: None, next: Instant::now(), entrada: None, pos: (0, 0), botones: 0 };
    el.run_app(&mut app).expect("bucle de eventos");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(w: u32, h: u32, bpr: u32) -> Vec<u8> {
        let mut v = vec![0u8; HDR_PAGE];
        v[0..4].copy_from_slice(b"TFB1");
        v[4..8].copy_from_slice(&w.to_le_bytes());
        v[8..12].copy_from_slice(&h.to_le_bytes());
        v[12..16].copy_from_slice(&bpr.to_le_bytes());
        v
    }

    #[test]
    fn cabecera_valida() {
        assert_eq!(parse_header(&page(64, 32, 256), 256 * 32), Some(Header { width: 64, height: 32, bytes_per_row: 256 }));
    }

    #[test]
    fn cabecera_rechaza_tamanos_imposibles() {
        assert_eq!(parse_header(&page(64, 100_000, 256), 256 * 32), None); // más alto que el archivo
        assert_eq!(parse_header(&page(64, 32, 100), 256 * 32), None); // fila más corta que el ancho
        assert_eq!(parse_header(&page(64, 32, u32::MAX), 256 * 32), None); // fila enorme: no cabe en el archivo
        assert_eq!(parse_header(b"corto", 0), None);
    }

    #[test]
    fn ruta_del_cursor() {
        assert_eq!(cursor_path(Path::new("/dev/shm/tahoe-fb-abc")), Some(PathBuf::from("/dev/shm/tahoe-fbcursor-abc")));
    }
}
