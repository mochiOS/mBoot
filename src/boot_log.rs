//! Bounded, printable log tail for pre-scanout boot diagnostics.

const COLUMNS: usize = 96;
const LINES: usize = 24;

pub struct ProbeReport {
    bytes: [u8; 4096],
    len: usize,
}

impl ProbeReport {
    pub const fn new() -> Self { Self { bytes: [0; 4096], len: 0 } }
    pub fn text(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).expect("only UTF-8 strings are appended")
    }
}

impl core::fmt::Write for ProbeReport {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        if text.len() > self.bytes.len() - self.len { return Err(core::fmt::Error); }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}

pub struct BootLog {
    bytes: [u8; (COLUMNS + 1) * LINES],
    len: usize,
    column: usize,
    lines: usize,
    failure_shown: bool,
    console_started: bool,
    console_finished: bool,
    rendered: [u8; COLUMNS * LINES],
}

impl BootLog {
    pub const fn new() -> Self {
        Self { bytes: [0; (COLUMNS + 1) * LINES], len: 0, column: 0, lines: 1, failure_shown: false,
            console_started: false, console_finished: false, rendered: [b' '; COLUMNS * LINES] }
    }

    fn newline(&mut self) {
        if self.lines == LINES {
            let end = self.bytes[..self.len].iter().position(|&b| b == b'\n').unwrap() + 1;
            self.bytes.copy_within(end..self.len, 0);
            self.len -= end;
        } else {
            self.lines += 1;
        }
        self.bytes[self.len] = b'\n';
        self.len += 1;
        self.column = 0;
    }

    pub fn append(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\r' { continue; }
            if byte == b'\n' {
                self.newline();
                continue;
            }
            if self.column == COLUMNS { self.newline(); }
            self.bytes[self.len] = match byte {
                b' '..=b'~' => byte,
                b'\t' => b' ',
                _ => b'?',
            };
            self.len += 1;
            self.column += 1;
        }
    }

    pub fn text(&self) -> &[u8] { &self.bytes[..self.len] }

    pub fn console_started(&self) -> bool { self.console_started }
    pub fn finish_console(&mut self) { self.console_finished = true; }
    pub fn console_finished(&self) -> bool { self.console_finished }

    pub fn render_console(&mut self, begin: impl FnOnce(), mut cell: impl FnMut(usize, usize, u8)) {
        if self.console_finished { return; }
        if !self.console_started {
            begin();
            self.console_started = true;
        }
        let mut next = [b' '; COLUMNS * LINES];
        let (mut row, mut col) = (0, 0);
        for &byte in self.text() {
            if byte == b'\n' { row += 1; col = 0; }
            else if row < LINES && col < COLUMNS { next[row * COLUMNS + col] = byte; col += 1; }
        }
        for (i, &byte) in next.iter().enumerate() {
            if self.rendered[i] != byte { cell(i % COLUMNS, i / COLUMNS, byte); }
        }
        self.rendered = next;
    }

    pub fn failure_shown(&self) -> bool { self.failure_shown }

    pub fn mark_failure_shown(&mut self) { self.failure_shown = true; }

    /// Show the first actionable failure once; ordinary printk must not redraw.
    pub fn take_failure(&mut self) -> bool {
        if self.failure_shown { return false; }
        let Some(end) = self.text().iter().rposition(|&b| b == b'\n') else { return false; };
        let complete = &self.text()[..=end];
        let failed = [
            b"Kernel panic".as_slice(),
            b"Oops:",
            b"BUG:",
            b"claim failed:",
            b"Initramfs unpacking failed:",
            b"CPU ISA level is lower than required",
            b"error while loading shared libraries:",
        ].iter().any(|needle| complete.windows(needle.len()).any(|part| part == *needle));
        self.failure_shown = failed;
        failed
    }
}

impl Default for BootLog {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_report_is_bounded_without_allocating() {
        use core::fmt::Write;
        let mut report = ProbeReport::new();
        report.write_str("GPU report\n").unwrap();
        write!(report, "errno={}\n", -5).unwrap();
        assert_eq!(report.text(), "GPU report\nerrno=-5\n");
        assert!(report.write_str(core::str::from_utf8(&[b'x'; 4096]).unwrap()).is_err());
        assert_eq!(report.text(), "GPU report\nerrno=-5\n");
    }

    #[test]
    fn preserves_fragmented_lines() {
        let mut log = BootLog::new();
        log.append(b"Linux ver");
        log.append(b"sion\r\nPANIC\t-5\n");
        assert_eq!(log.text(), b"Linux version\nPANIC -5\n");
    }

    #[test]
    fn console_updates_only_changed_cells_and_preserves_case() {
        let mut log = BootLog::new();
        log.append(b"Linux\n");
        let mut cells = [(0, 0, 0); 5];
        let mut count = 0;
        log.render_console(|| {}, |x, y, ch| { cells[count] = (x, y, ch); count += 1; });
        assert_eq!(count, 5);
        assert_eq!(cells[1], (1, 0, b'i'));
        log.render_console(|| panic!("must not clear again"), |_, _, _| panic!("unchanged"));
        log.append(b"ok");
        count = 0;
        log.render_console(|| panic!("must not clear again"), |x, y, ch| { cells[count] = (x, y, ch); count += 1; });
        assert_eq!(count, 2);
        assert_eq!(&cells[..count], &[(0, 1, b'o'), (1, 1, b'k')]);
    }

    #[test]
    fn console_never_writes_after_linux_handoff() {
        let mut log = BootLog::new();
        log.append(b"boot\n");
        log.finish_console();
        log.render_console(|| panic!("handoff"), |_, _, _| panic!("handoff"));
        assert!(log.console_finished());
    }

    #[test]
    fn ordinary_logs_never_request_a_redraw() {
        let mut log = BootLog::new();
        for _ in 0..1000 {
            log.append(b"device initialized\nfirmware load failed -2\n");
            assert!(!log.take_failure());
        }
    }

    #[test]
    fn structured_failure_stays_latched() {
        let mut log = BootLog::new();
        log.mark_failure_shown();
        for _ in 0..100 {
            log.append(b"ordinary startup status\nKernel panic: later error\n");
            assert!(!log.take_failure());
            assert!(log.failure_shown());
        }
    }

    #[test]
    fn completed_failure_is_displayed_only_once() {
        let mut log = BootLog::new();
        log.append(b"Kernel pan");
        assert!(!log.take_failure());
        log.append(b"ic: cannot mount root");
        assert!(!log.take_failure());
        log.append(b"\n");
        assert!(log.take_failure());
        for _ in 0..1000 {
            log.append(b"error while loading shared libraries: libdrm.so.2\n");
            assert!(!log.take_failure());
        }
        assert!(log.failure_shown());
    }

    #[test]
    fn wraps_and_bounds_long_records() {
        let mut log = BootLog::new();
        for _ in 0..1000 { log.append(&[b'x'; 137]); }
        assert!(log.text().len() <= (COLUMNS + 1) * LINES);
        assert!(log.text().split(|&b| b == b'\n').count() <= LINES);
        assert!(log.text().split(|&b| b == b'\n').all(|line| line.len() <= COLUMNS));
    }

    #[test]
    fn keeps_latest_records_and_sanitizes_controls() {
        let mut log = BootLog::new();
        log.append(b"old\n");
        for _ in 0..100 { log.append(b"current\n"); }
        log.append(b"error\x1b\xff");
        assert!(log.text().starts_with(b"current\n"));
        assert!(log.text().ends_with(b"error??"));
    }
}
