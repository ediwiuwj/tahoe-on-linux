//! Teclado y ratón hacia macOS. tahoe-run (dentro de WindowServer, que en macOS lleva el sistema de eventos HID) escucha
//! en un socket Unix de datagramas, `/dev/shm/tahoe-input-<id>` (el mismo `<id>` que la memoria de vídeo), y convierte
//! cada mensaje en un evento de los servicios IOHIDEventDriver que publica (ver `tahoe-run/emu_iokit.inc`).
//!
//! Mensaje: 16 bytes, cuatro enteros de 32 bits en little endian `[tipo, a, b, c]`:
//! - 1 tecla: a = página de uso HID (7, teclado), b = uso, c = 1 pulsada / 0 soltada.
//! - 2 puntero: a = x, b = y en píxeles de la pantalla de macOS, c = botones (bit 0 izquierdo, 1 derecho, 2 central).
//! - 3 rueda: a = desplazamiento horizontal, b = vertical (líneas; positivo hacia arriba / a la izquierda).

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};

use winit::keyboard::KeyCode;

pub const TECLA: u32 = 1;
pub const PUNTERO: u32 = 2;
pub const RUEDA: u32 = 3;

/// El socket de entrada del mismo arranque: tahoe-fb-<id> -> tahoe-input-<id>.
pub fn ruta_entrada(fb: &Path) -> Option<PathBuf> {
    let name = fb.file_name()?.to_str()?;
    Some(fb.with_file_name(format!("tahoe-input-{}", name.strip_prefix("tahoe-fb-")?)))
}

pub fn mensaje(tipo: u32, a: i32, b: i32, c: i32) -> [u8; 16] {
    let mut m = [0u8; 16];
    m[0..4].copy_from_slice(&tipo.to_le_bytes());
    m[4..8].copy_from_slice(&a.to_le_bytes());
    m[8..12].copy_from_slice(&b.to_le_bytes());
    m[12..16].copy_from_slice(&c.to_le_bytes());
    m
}

/// Envía a tahoe-run; si macOS aún no escucha (o WindowServer se reinició), el mensaje se pierde sin más.
pub struct Entrada {
    sock: Option<UnixDatagram>,
    ruta: PathBuf,
}

impl Entrada {
    pub fn new(ruta: PathBuf) -> Entrada {
        Entrada { sock: UnixDatagram::unbound().ok(), ruta }
    }
    pub fn enviar(&self, tipo: u32, a: i32, b: i32, c: i32) {
        if let Some(s) = &self.sock {
            let _ = s.send_to(&mensaje(tipo, a, b, c), &self.ruta);
        }
    }
}

/// Uso HID (página 7, teclado) de una tecla física. La distribución (español, etc.) la aplica macOS, como con un
/// teclado USB de verdad: aquí solo va la posición de la tecla.
pub fn uso_hid(k: KeyCode) -> Option<u32> {
    use KeyCode::*;
    let u = match k {
        KeyA => 0x04, KeyB => 0x05, KeyC => 0x06, KeyD => 0x07, KeyE => 0x08, KeyF => 0x09, KeyG => 0x0a,
        KeyH => 0x0b, KeyI => 0x0c, KeyJ => 0x0d, KeyK => 0x0e, KeyL => 0x0f, KeyM => 0x10, KeyN => 0x11,
        KeyO => 0x12, KeyP => 0x13, KeyQ => 0x14, KeyR => 0x15, KeyS => 0x16, KeyT => 0x17, KeyU => 0x18,
        KeyV => 0x19, KeyW => 0x1a, KeyX => 0x1b, KeyY => 0x1c, KeyZ => 0x1d,
        Digit1 => 0x1e, Digit2 => 0x1f, Digit3 => 0x20, Digit4 => 0x21, Digit5 => 0x22, Digit6 => 0x23,
        Digit7 => 0x24, Digit8 => 0x25, Digit9 => 0x26, Digit0 => 0x27,
        Enter => 0x28, Escape => 0x29, Backspace => 0x2a, Tab => 0x2b, Space => 0x2c, Minus => 0x2d,
        Equal => 0x2e, BracketLeft => 0x2f, BracketRight => 0x30, Backslash => 0x31, Semicolon => 0x33,
        Quote => 0x34, Backquote => 0x35, Comma => 0x36, Period => 0x37, Slash => 0x38, CapsLock => 0x39,
        F1 => 0x3a, F2 => 0x3b, F3 => 0x3c, F4 => 0x3d, F5 => 0x3e, F6 => 0x3f, F7 => 0x40, F8 => 0x41,
        F9 => 0x42, F10 => 0x43, F11 => 0x44,
        PrintScreen => 0x46, ScrollLock => 0x47, Pause => 0x48, Insert => 0x49, Home => 0x4a, PageUp => 0x4b,
        Delete => 0x4c, End => 0x4d, PageDown => 0x4e, ArrowRight => 0x4f, ArrowLeft => 0x50,
        ArrowDown => 0x51, ArrowUp => 0x52, NumLock => 0x53, NumpadDivide => 0x54, NumpadMultiply => 0x55,
        NumpadSubtract => 0x56, NumpadAdd => 0x57, NumpadEnter => 0x58, Numpad1 => 0x59, Numpad2 => 0x5a,
        Numpad3 => 0x5b, Numpad4 => 0x5c, Numpad5 => 0x5d, Numpad6 => 0x5e, Numpad7 => 0x5f, Numpad8 => 0x60,
        Numpad9 => 0x61, Numpad0 => 0x62, NumpadDecimal => 0x63,
        IntlBackslash => 0x64, // la tecla < > de los teclados ISO (español)
        ContextMenu => 0x65,
        ControlLeft => 0xe0, ShiftLeft => 0xe1, AltLeft => 0xe2, SuperLeft => 0xe3, // Super (Windows) = Comando
        ControlRight => 0xe4, ShiftRight => 0xe5, AltRight => 0xe6, SuperRight => 0xe7,
        _ => return None,
    };
    Some(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruta() {
        assert_eq!(ruta_entrada(Path::new("/dev/shm/tahoe-fb-abc")), Some(PathBuf::from("/dev/shm/tahoe-input-abc")));
        assert_eq!(ruta_entrada(Path::new("/dev/shm/otra")), None);
    }

    #[test]
    fn formato() {
        assert_eq!(mensaje(TECLA, 7, 0x04, 1), [1, 0, 0, 0, 7, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&mensaje(PUNTERO, -1, 2, 0)[4..8], &[0xff; 4]);
    }

    #[test]
    fn teclas() {
        assert_eq!(uso_hid(KeyCode::KeyA), Some(0x04));
        assert_eq!(uso_hid(KeyCode::Enter), Some(0x28));
        assert_eq!(uso_hid(KeyCode::SuperLeft), Some(0xe3));
        assert_eq!(uso_hid(KeyCode::IntlBackslash), Some(0x64));
        assert_eq!(uso_hid(KeyCode::F12), None); // F12 cierra el visor
    }
}
