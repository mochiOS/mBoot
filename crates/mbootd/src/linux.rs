use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::xproto::{
    AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, EventMask, ImageFormat, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT, MapState, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::linux_portal::PortalMount;
use crate::linux_sandbox::{LinuxSandbox, SandboxError};

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_CHUNK: usize = 1536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxError {
    Busy,
    InvalidArgument,
    InvalidState,
    NotFound,
    Internal,
}

struct LinuxInstance {
    child: Child,
    display: Option<InstanceDisplay>,
    _sandbox: Option<LinuxSandbox>,
}

struct InstanceDisplay {
    connection: RustConnection,
    screen: usize,
    _name: String,
    xvfb: Child,
}

impl Drop for InstanceDisplay {
    fn drop(&mut self) {
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
    }
}

impl Drop for LinuxInstance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct CachedFrame {
    generation: u64,
    bytes: Vec<u8>,
}

pub(crate) struct WindowInfo {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) generation: u64,
    pub(crate) frame_size: usize,
    pub(crate) encoded_size: usize,
    pub(crate) title: Vec<u8>,
}

pub(crate) struct FrameChunk {
    pub(crate) total_size: usize,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct LinuxBridge {
    connection: Option<RustConnection>,
    screen: usize,
    display: String,
    instances: HashMap<u64, LinuxInstance>,
    frames: HashMap<(u64, Window), CachedFrame>,
    next_generation: u64,
}

impl Default for LinuxBridge {
    fn default() -> Self {
        let display = env::var("MBOOT_LINUX_DISPLAY")
            .or_else(|_| env::var("DISPLAY"))
            .unwrap_or_else(|_| String::from(":0"));
        let (connection, screen) = connect_display(&display).unwrap_or((None, 0));
        Self {
            connection,
            screen,
            display,
            instances: HashMap::new(),
            frames: HashMap::new(),
            next_generation: 1,
        }
    }
}

impl LinuxBridge {
    pub(crate) fn launch(&mut self, instance: u64, application: &str) -> Result<u32, LinuxError> {
        if instance == 0 || self.instances.contains_key(&instance) {
            return Err(if instance == 0 {
                LinuxError::InvalidArgument
            } else {
                LinuxError::Busy
            });
        }
        self.connection()?;
        let executable = application_executable(application).ok_or(LinuxError::NotFound)?;
        let child = Command::new(executable)
            .env("DISPLAY", &self.display)
            .args(["-name", &format!("mochios-linux-{instance}")])
            .spawn()
            .map_err(|_| LinuxError::Internal)?;
        let pid = child.id();
        self.instances.insert(
            instance,
            LinuxInstance {
                child,
                display: None,
                _sandbox: None,
            },
        );
        Ok(pid)
    }

    pub(crate) fn launch_bundle(
        &mut self,
        instance: u64,
        bundle: &str,
        entrypoint: &str,
        user: &str,
        writable: &str,
        portal_mounts: &[PortalMount],
    ) -> Result<u32, LinuxError> {
        if instance == 0 || self.instances.contains_key(&instance) {
            return Err(if instance == 0 {
                LinuxError::InvalidArgument
            } else {
                LinuxError::Busy
            });
        }
        let mut sandbox = LinuxSandbox::prepare(instance, bundle, user, writable, portal_mounts)
            .map_err(sandbox_error)?;
        let display_name = format!(":{}", 1000 + instance % 50_000);
        let display_number = display_name
            .strip_prefix(':')
            .ok_or(LinuxError::InvalidArgument)?;
        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display_number}"));
        let mut xvfb = Command::new("Xvfb")
            .arg(&display_name)
            .args([
                "-screen",
                "0",
                "1280x800x24",
                "-nolisten",
                "tcp",
                "-noreset",
                "-ac",
            ])
            .spawn()
            .map_err(|_| LinuxError::Internal)?;
        let (connection, screen) = match wait_for_display(&display_name, &socket, &mut xvfb) {
            Ok(display) => display,
            Err(error) => {
                let _ = xvfb.kill();
                let _ = xvfb.wait();
                return Err(error);
            }
        };
        if let Err(error) = sandbox.expose_x11(&socket) {
            let _ = xvfb.kill();
            let _ = xvfb.wait();
            return Err(sandbox_error(error));
        }
        let child = match sandbox.launch(entrypoint, &display_name, instance) {
            Ok(child) => child,
            Err(error) => {
                let _ = xvfb.kill();
                let _ = xvfb.wait();
                return Err(sandbox_error(error));
            }
        };
        let pid = child.id();
        self.instances.insert(
            instance,
            LinuxInstance {
                child,
                display: Some(InstanceDisplay {
                    connection,
                    screen,
                    _name: display_name,
                    xvfb,
                }),
                _sandbox: Some(sandbox),
            },
        );
        Ok(pid)
    }

    pub(crate) fn windows(&mut self, instance: u64) -> Result<Vec<Window>, LinuxError> {
        let pid = self.live_pid(instance)?;
        let (connection, screen_index) = self.connection_for(instance)?;
        let screen = connection
            .setup()
            .roots
            .get(screen_index)
            .ok_or(LinuxError::InvalidState)?;
        let pid_atom = connection
            .intern_atom(false, b"_NET_WM_PID")
            .map_err(|_| LinuxError::Internal)?
            .reply()
            .map_err(|_| LinuxError::Internal)?
            .atom;
        let children = connection
            .query_tree(screen.root)
            .map_err(|_| LinuxError::Internal)?
            .reply()
            .map_err(|_| LinuxError::Internal)?
            .children;
        let mut windows = Vec::new();
        for window in children {
            let attributes = match connection.get_window_attributes(window) {
                Ok(cookie) => match cookie.reply() {
                    Ok(attributes) => attributes,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            if attributes.map_state == MapState::UNMAPPED {
                continue;
            }
            if window_matches_instance(connection, window, instance, pid_atom, pid) {
                windows.push(window);
            }
        }
        self.frames
            .retain(|(owner, window), _| *owner != instance || windows.contains(window));
        Ok(windows)
    }

    pub(crate) fn window_info(
        &mut self,
        instance: u64,
        window: Window,
    ) -> Result<WindowInfo, LinuxError> {
        self.require_owned_window(instance, window)?;
        let (width, height, bytes, title) = {
            let (connection, _) = self.connection_for(instance)?;
            let geometry = connection
                .get_geometry(window)
                .map_err(|_| LinuxError::NotFound)?
                .reply()
                .map_err(|_| LinuxError::NotFound)?;
            if geometry.width == 0 || geometry.height == 0 {
                return Err(LinuxError::InvalidState);
            }
            let pixmap = connection.generate_id().map_err(|_| LinuxError::Internal)?;
            connection
                .composite_name_window_pixmap(window, pixmap)
                .map_err(|_| LinuxError::Internal)?
                .check()
                .map_err(|_| LinuxError::Internal)?;
            let image = connection
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    pixmap,
                    0,
                    0,
                    geometry.width,
                    geometry.height,
                    u32::MAX,
                )
                .map_err(|_| LinuxError::Internal)?
                .reply()
                .map_err(|_| LinuxError::Internal);
            let _ = connection.free_pixmap(pixmap);
            let image = image?;
            (
                geometry.width,
                geometry.height,
                image.data,
                window_title(connection, window),
            )
        };
        let expected = usize::from(width)
            .checked_mul(usize::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(LinuxError::InvalidArgument)?;
        if expected > MAX_FRAME_BYTES || bytes.len() < expected {
            return Err(LinuxError::InvalidState);
        }
        let encoded = encode_rle32(&bytes[..expected])?;
        let encoded_size = encoded.len();
        let generation = self.cache_frame((instance, window), encoded);
        Ok(WindowInfo {
            width,
            height,
            generation,
            frame_size: expected,
            encoded_size,
            title,
        })
    }

    pub(crate) fn frame(
        &self,
        instance: u64,
        window: Window,
        generation: u64,
        offset: u64,
        maximum: u64,
    ) -> Result<FrameChunk, LinuxError> {
        let frame = self
            .frames
            .get(&(instance, window))
            .filter(|frame| frame.generation == generation)
            .ok_or(LinuxError::InvalidState)?;
        let offset = usize::try_from(offset).map_err(|_| LinuxError::InvalidArgument)?;
        let maximum = usize::try_from(maximum)
            .map_err(|_| LinuxError::InvalidArgument)?
            .min(MAX_FRAME_CHUNK);
        if maximum == 0 || offset > frame.bytes.len() {
            return Err(LinuxError::InvalidArgument);
        }
        let end = offset.saturating_add(maximum).min(frame.bytes.len());
        Ok(FrameChunk {
            total_size: frame.bytes.len(),
            bytes: frame.bytes[offset..end].to_vec(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn input(
        &mut self,
        instance: u64,
        window: Window,
        kind: &str,
        code: u8,
        value: i32,
        x: i16,
        y: i16,
    ) -> Result<(), LinuxError> {
        self.require_owned_window(instance, window)?;
        let (connection, screen_index) = self.connection_for(instance)?;
        let root = connection
            .setup()
            .roots
            .get(screen_index)
            .ok_or(LinuxError::InvalidState)?
            .root;
        let translated = connection
            .translate_coordinates(window, root, x, y)
            .map_err(|_| LinuxError::Internal)?
            .reply()
            .map_err(|_| LinuxError::Internal)?;
        match kind {
            "motion" => connection.xtest_fake_input(
                MOTION_NOTIFY_EVENT,
                0,
                CURRENT_TIME,
                root,
                translated.dst_x,
                translated.dst_y,
                0,
            ),
            "button" => connection.xtest_fake_input(
                if value == 0 {
                    BUTTON_RELEASE_EVENT
                } else {
                    BUTTON_PRESS_EVENT
                },
                code,
                CURRENT_TIME,
                root,
                translated.dst_x,
                translated.dst_y,
                0,
            ),
            "key" => connection.xtest_fake_input(
                if value == 0 {
                    KEY_RELEASE_EVENT
                } else {
                    KEY_PRESS_EVENT
                },
                code,
                CURRENT_TIME,
                root,
                0,
                0,
                0,
            ),
            "scroll" => {
                let button = if value < 0 { 4 } else { 5 };
                connection
                    .xtest_fake_input(
                        BUTTON_PRESS_EVENT,
                        button,
                        CURRENT_TIME,
                        root,
                        translated.dst_x,
                        translated.dst_y,
                        0,
                    )
                    .and_then(|_| {
                        connection.xtest_fake_input(
                            BUTTON_RELEASE_EVENT,
                            button,
                            CURRENT_TIME,
                            root,
                            translated.dst_x,
                            translated.dst_y,
                            0,
                        )
                    })
            }
            "focus" => connection.set_input_focus(
                x11rb::protocol::xproto::InputFocus::PARENT,
                if value == 0 { root } else { window },
                CURRENT_TIME,
            ),
            _ => return Err(LinuxError::InvalidArgument),
        }
        .map_err(|_| LinuxError::Internal)?;
        connection.flush().map_err(|_| LinuxError::Internal)
    }

    pub(crate) fn configure(
        &mut self,
        instance: u64,
        window: Window,
        width: u16,
        height: u16,
    ) -> Result<(), LinuxError> {
        if width == 0 || height == 0 {
            return Err(LinuxError::InvalidArgument);
        }
        self.require_owned_window(instance, window)?;
        let (connection, _) = self.connection_for(instance)?;
        connection
            .configure_window(
                window,
                &ConfigureWindowAux::new()
                    .width(u32::from(width))
                    .height(u32::from(height)),
            )
            .map_err(|_| LinuxError::Internal)?;
        connection.flush().map_err(|_| LinuxError::Internal)
    }

    pub(crate) fn close(&mut self, instance: u64, window: Window) -> Result<(), LinuxError> {
        self.require_owned_window(instance, window)?;
        let (connection, _) = self.connection_for(instance)?;
        let wm_protocols = connection
            .intern_atom(false, b"WM_PROTOCOLS")
            .map_err(|_| LinuxError::Internal)?
            .reply()
            .map_err(|_| LinuxError::Internal)?
            .atom;
        let wm_delete = connection
            .intern_atom(false, b"WM_DELETE_WINDOW")
            .map_err(|_| LinuxError::Internal)?
            .reply()
            .map_err(|_| LinuxError::Internal)?
            .atom;
        let event = ClientMessageEvent::new(32, window, wm_protocols, [wm_delete, 0, 0, 0, 0]);
        connection
            .send_event(false, window, EventMask::NO_EVENT, event)
            .map_err(|_| LinuxError::Internal)?;
        connection.flush().map_err(|_| LinuxError::Internal)
    }

    fn connection(&self) -> Result<&RustConnection, LinuxError> {
        self.connection.as_ref().ok_or(LinuxError::InvalidState)
    }

    fn connection_for(&self, instance: u64) -> Result<(&RustConnection, usize), LinuxError> {
        match self
            .instances
            .get(&instance)
            .and_then(|instance| instance.display.as_ref())
        {
            Some(display) => Ok((&display.connection, display.screen)),
            None => Ok((self.connection()?, self.screen)),
        }
    }

    fn live_pid(&mut self, instance: u64) -> Result<u32, LinuxError> {
        let state = self
            .instances
            .get_mut(&instance)
            .ok_or(LinuxError::NotFound)?;
        match state.child.try_wait() {
            Ok(None) => Ok(state.child.id()),
            Ok(Some(_)) | Err(_) => {
                self.instances.remove(&instance);
                self.frames.retain(|(owner, _), _| *owner != instance);
                Err(LinuxError::NotFound)
            }
        }
    }

    fn require_owned_window(&mut self, instance: u64, window: Window) -> Result<(), LinuxError> {
        self.windows(instance)?
            .contains(&window)
            .then_some(())
            .ok_or(LinuxError::NotFound)
    }

    fn allocate_generation(&mut self) -> u64 {
        loop {
            let generation = self.next_generation;
            self.next_generation = self.next_generation.wrapping_add(1);
            if generation != 0 {
                return generation;
            }
        }
    }

    fn cache_frame(&mut self, key: (u64, Window), encoded: Vec<u8>) -> u64 {
        if let Some(frame) = self.frames.get(&key).filter(|frame| frame.bytes == encoded) {
            return frame.generation;
        }
        let generation = self.allocate_generation();
        self.frames.insert(
            key,
            CachedFrame {
                generation,
                bytes: encoded,
            },
        );
        generation
    }
}

fn connect_display(display: &str) -> Result<(Option<RustConnection>, usize), LinuxError> {
    let (connection, screen) =
        x11rb::connect(Some(display)).map_err(|_| LinuxError::InvalidState)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or(LinuxError::InvalidState)?
        .root;
    connection
        .composite_redirect_subwindows(root, Redirect::AUTOMATIC)
        .map_err(|_| LinuxError::InvalidState)?
        .check()
        .map_err(|_| LinuxError::InvalidState)?;
    connection.flush().map_err(|_| LinuxError::InvalidState)?;
    Ok((Some(connection), screen))
}

fn wait_for_display(
    display: &str,
    socket: &std::path::Path,
    xvfb: &mut Child,
) -> Result<(RustConnection, usize), LinuxError> {
    for _ in 0..100 {
        if xvfb.try_wait().map_err(|_| LinuxError::Internal)?.is_some() {
            return Err(LinuxError::Internal);
        }
        if socket.exists() {
            if let Ok((Some(connection), screen)) = connect_display(display) {
                return Ok((connection, screen));
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(LinuxError::Internal)
}

fn sandbox_error(error: SandboxError) -> LinuxError {
    match error {
        SandboxError::InvalidArgument => LinuxError::InvalidArgument,
        SandboxError::NotFound => LinuxError::NotFound,
        SandboxError::Internal => LinuxError::Internal,
    }
}

fn window_matches_instance(
    connection: &RustConnection,
    window: Window,
    instance: u64,
    pid_atom: u32,
    pid: u32,
) -> bool {
    let expected_class = format!("mochios-linux-{instance}");
    if connection
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|property| {
            wm_class_instance(&property.value) == Some(expected_class.as_bytes())
        })
    {
        return true;
    }
    connection
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .and_then(|property| property.value32().and_then(|mut values| values.next()))
        == Some(pid)
}

fn wm_class_instance(value: &[u8]) -> Option<&[u8]> {
    let end = value.iter().position(|byte| *byte == 0)?;
    (end != 0).then_some(&value[..end])
}

fn application_executable(application: &str) -> Option<&'static str> {
    match application {
        "xterm" => Some("/usr/bin/xterm"),
        "xcalc" => Some("/usr/bin/xcalc"),
        "xclock" => Some("/usr/bin/xclock"),
        _ => None,
    }
}

fn window_title(connection: &RustConnection, window: Window) -> Vec<u8> {
    let Ok(property) =
        connection.get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)
    else {
        return Vec::new();
    };
    let raw = property
        .reply()
        .map_or_else(|_| Vec::new(), |reply| reply.value);
    let title = String::from_utf8_lossy(&raw);
    let mut end = title.len().min(64);
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        b"Linux application".to_vec()
    } else {
        title.as_bytes()[..end].to_vec()
    }
}

fn encode_rle32(frame: &[u8]) -> Result<Vec<u8>, LinuxError> {
    if frame.len() % 4 != 0 {
        return Err(LinuxError::InvalidState);
    }
    let mut encoded = Vec::new();
    encoded
        .try_reserve(frame.len().min(64 * 1024))
        .map_err(|_| LinuxError::Internal)?;
    let mut pixels = frame.chunks_exact(4).peekable();
    while let Some(pixel) = pixels.next() {
        let mut count = 1u16;
        while count < u16::MAX && pixels.peek().is_some_and(|candidate| *candidate == pixel) {
            let _ = pixels.next();
            count += 1;
        }
        encoded.extend_from_slice(&count.to_le_bytes());
        encoded.extend_from_slice(pixel);
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_allowlist_does_not_accept_paths_or_shells() {
        assert_eq!(application_executable("xterm"), Some("/usr/bin/xterm"));
        assert_eq!(application_executable("xcalc"), Some("/usr/bin/xcalc"));
        assert_eq!(application_executable("xclock"), Some("/usr/bin/xclock"));
        assert_eq!(application_executable("/bin/sh"), None);
        assert_eq!(application_executable("xterm;id"), None);
    }

    #[test]
    fn wm_class_uses_the_instance_component() {
        assert_eq!(
            wm_class_instance(b"mochios-linux-12\0XTerm\0"),
            Some(b"mochios-linux-12".as_slice())
        );
        assert_eq!(wm_class_instance(b"\0XTerm\0"), None);
        assert_eq!(wm_class_instance(b"unterminated"), None);
    }

    #[test]
    fn frame_chunks_are_bounded_and_generation_checked() {
        let mut bridge = LinuxBridge::default();
        bridge.frames.insert(
            (9, 12),
            CachedFrame {
                generation: 3,
                bytes: vec![1; 4096],
            },
        );
        assert_eq!(bridge.frame(9, 12, 3, 0, 4096).unwrap().bytes.len(), 1536);
        assert!(matches!(
            bridge.frame(9, 12, 4, 0, 1),
            Err(LinuxError::InvalidState)
        ));
    }

    #[test]
    fn rle32_compacts_solid_pixels_without_losing_run_boundaries() {
        let mut frame = vec![0x11; usize::from(u16::MAX) * 4];
        frame.extend_from_slice(&[0x22; 8]);
        let encoded = encode_rle32(&frame).unwrap();
        assert_eq!(&encoded[..2], &u16::MAX.to_le_bytes());
        assert_eq!(&encoded[2..6], &[0x11; 4]);
        assert_eq!(&encoded[6..8], &2u16.to_le_bytes());
        assert_eq!(&encoded[8..12], &[0x22; 4]);
    }

    #[test]
    fn identical_frames_reuse_generation_and_changed_frames_advance_it() {
        let mut bridge = LinuxBridge::default();
        let first = bridge.cache_frame((4, 8), vec![1, 2, 3]);
        let unchanged = bridge.cache_frame((4, 8), vec![1, 2, 3]);
        let changed = bridge.cache_frame((4, 8), vec![1, 2, 4]);
        assert_eq!(unchanged, first);
        assert_ne!(changed, first);
    }
}
