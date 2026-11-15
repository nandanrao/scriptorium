//! `vpointer`: move, click and scroll a virtual pointer on a wlroots compositor.
//!
//! Headless Sway has no pointer device, so GUI tests can't click anything with
//! `swaymsg seat - cursor`. This tool creates a `zwlr_virtual_pointer_v1` device
//! for the length of one command, which gives the seat a pointer, and sends the
//! events through it. Coordinates are in the first output's pixels: the same
//! coordinates as a `grim` screenshot of that output, so a target can be read
//! off a shot.
//!
//! ```text
//! vpointer move X Y                 move the pointer to (X, Y)
//! vpointer click X Y [left|right|middle] [--double]
//!                                   move there, then press and release
//! vpointer scroll X Y DY            move there, then scroll DY lines (+ is down)
//! ```
//!
//! Build: `cargo build -p scriptorium-app --example vpointer`
//! (binary at `target/debug/examples/vpointer`). Like every display command in
//! these tests, run it with `WAYLAND_DISPLAY=wayland-N` for the private
//! compositor on the same command line; it refuses to run without one set.
//!
//! Exit status: 0 on success, 2 for a usage error, 1 if the compositor lacks
//! the protocol or the connection fails.

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(linux::run(std::env::args().skip(1).collect()));
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("vpointer: Wayland only");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::{Duration, Instant};

    use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat};
    use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
    use wayland_protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    };

    const USAGE: &str = "usage: vpointer move X Y
       vpointer click X Y [left|right|middle] [--double]
       vpointer scroll X Y DY";

    /// Linux input event codes (linux/input-event-codes.h).
    const BTN_LEFT: u32 = 0x110;
    const BTN_RIGHT: u32 = 0x111;
    const BTN_MIDDLE: u32 = 0x112;

    /// Lets the client see the pointer arrive (enter + motion, so hover state is
    /// current) before a button goes down; GPUI decides what a press hits from
    /// the last motion it saw.
    const SETTLE: Duration = Duration::from_millis(60);
    /// Between the two clicks of a double click: well inside any double-click time.
    const DOUBLE_GAP: Duration = Duration::from_millis(80);

    #[derive(Debug, PartialEq)]
    enum Cmd {
        Move { x: f64, y: f64 },
        Click { x: f64, y: f64, button: u32, double: bool },
        Scroll { x: f64, y: f64, lines: i32 },
    }

    fn parse(args: &[String]) -> Result<Cmd, String> {
        let num = |i: usize, what: &str| -> Result<f64, String> {
            let s = args.get(i).ok_or_else(|| format!("missing {what}"))?;
            s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(|| format!("bad {what}: {s}"))
        };
        match args.first().map(String::as_str) {
            Some("move") if args.len() == 3 => Ok(Cmd::Move { x: num(1, "X")?, y: num(2, "Y")? }),
            Some("click") if (3..=5).contains(&args.len()) => {
                let (mut button, mut double) = (BTN_LEFT, false);
                for a in &args[3..] {
                    match a.as_str() {
                        "left" => button = BTN_LEFT,
                        "right" => button = BTN_RIGHT,
                        "middle" => button = BTN_MIDDLE,
                        "--double" => double = true,
                        other => return Err(format!("unknown click option: {other}")),
                    }
                }
                Ok(Cmd::Click { x: num(1, "X")?, y: num(2, "Y")?, button, double })
            }
            Some("scroll") if args.len() == 4 => {
                let lines = args[3].parse::<i32>().map_err(|_| format!("bad DY: {}", args[3]))?;
                Ok(Cmd::Scroll { x: num(1, "X")?, y: num(2, "Y")?, lines })
            }
            _ => Err(USAGE.to_string()),
        }
    }

    #[derive(Default)]
    struct State {
        manager: Option<ZwlrVirtualPointerManagerV1>,
        seat: Option<wl_seat::WlSeat>,
        output: Option<wl_output::WlOutput>,
        /// The output's current mode, in pixels.
        size: Option<(u32, u32)>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            let wl_registry::Event::Global { name, interface, version } = event else { return };
            match interface.as_str() {
                "zwlr_virtual_pointer_manager_v1" if state.manager.is_none() => {
                    state.manager = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "wl_seat" if state.seat.is_none() => state.seat = Some(registry.bind(name, version.min(1), qh, ())),
                // The first output only: tests run one headless output.
                "wl_output" if state.output.is_none() => {
                    state.output = Some(registry.bind(name, version.min(2), qh, ()))
                }
                _ => {}
            }
        }
    }

    impl Dispatch<wl_output::WlOutput, ()> for State {
        fn event(
            state: &mut Self,
            _: &wl_output::WlOutput,
            event: wl_output::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_output::Event::Mode { flags: WEnum::Value(flags), width, height, .. } = event
                && flags.contains(wl_output::Mode::Current)
            {
                state.size = Some((width.max(1) as u32, height.max(1) as u32));
            }
        }
    }

    wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
    wayland_client::delegate_noop!(State: ZwlrVirtualPointerManagerV1);
    wayland_client::delegate_noop!(State: ZwlrVirtualPointerV1);

    /// Milliseconds since the tool started, for event timestamps.
    fn now(t0: Instant) -> u32 {
        t0.elapsed().as_millis() as u32
    }

    fn flush(queue: &mut EventQueue<State>, state: &mut State) -> Result<(), String> {
        queue.roundtrip(state).map(|_| ()).map_err(|e| format!("wayland: {e}"))
    }

    pub fn run(args: Vec<String>) -> i32 {
        let cmd = match parse(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("vpointer: {e}");
                return 2;
            }
        };
        // Never fall back to the user's session: a test must name its display.
        if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()) {
            eprintln!("vpointer: set WAYLAND_DISPLAY to the test compositor's socket");
            return 2;
        }
        match send(cmd) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("vpointer: {e}");
                1
            }
        }
    }

    fn send(cmd: Cmd) -> Result<(), String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        flush(&mut queue, &mut state)?; // globals
        flush(&mut queue, &mut state)?; // output modes
        let manager = state.manager.clone().ok_or("compositor has no zwlr_virtual_pointer_manager_v1")?;
        let (w, h) = state.size.ok_or("no output with a current mode")?;
        let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
        let t0 = Instant::now();

        let (x, y) = match cmd {
            Cmd::Move { x, y } | Cmd::Click { x, y, .. } | Cmd::Scroll { x, y, .. } => (x, y),
        };
        if x >= w as f64 || y >= h as f64 {
            return Err(format!("({x}, {y}) is outside the {w}x{h} output"));
        }
        // motion_absolute takes integer positions within an extent; using the
        // output size as the extent makes them pixels.
        pointer.motion_absolute(now(t0), x.round() as u32, y.round() as u32, w, h);
        pointer.frame();
        flush(&mut queue, &mut state)?;
        std::thread::sleep(SETTLE);

        match cmd {
            Cmd::Move { .. } => {}
            Cmd::Click { button, double, .. } => {
                for i in 0..if double { 2 } else { 1 } {
                    if i > 0 {
                        std::thread::sleep(DOUBLE_GAP);
                    }
                    pointer.button(now(t0), button, wl_pointer::ButtonState::Pressed);
                    pointer.frame();
                    flush(&mut queue, &mut state)?;
                    std::thread::sleep(Duration::from_millis(30));
                    pointer.button(now(t0), button, wl_pointer::ButtonState::Released);
                    pointer.frame();
                    flush(&mut queue, &mut state)?;
                }
            }
            Cmd::Scroll { lines, .. } => {
                // One wheel detent is 15 units of axis value in libinput terms.
                pointer.axis_source(wl_pointer::AxisSource::Wheel);
                pointer.axis_discrete(now(t0), wl_pointer::Axis::VerticalScroll, 15.0 * f64::from(lines), lines);
                pointer.frame();
                flush(&mut queue, &mut state)?;
            }
        }
        // Give the compositor time to deliver the last frame before the device
        // goes away with the connection.
        std::thread::sleep(SETTLE);
        pointer.destroy();
        flush(&mut queue, &mut state)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(s: &str) -> Vec<String> {
            s.split_whitespace().map(String::from).collect()
        }

        #[test]
        fn parses_commands() {
            assert_eq!(parse(&args("move 10 20.5")), Ok(Cmd::Move { x: 10.0, y: 20.5 }));
            assert_eq!(parse(&args("click 1 2")), Ok(Cmd::Click { x: 1.0, y: 2.0, button: BTN_LEFT, double: false }));
            assert_eq!(
                parse(&args("click 1 2 right --double")),
                Ok(Cmd::Click { x: 1.0, y: 2.0, button: BTN_RIGHT, double: true })
            );
            assert_eq!(parse(&args("scroll 5 6 -3")), Ok(Cmd::Scroll { x: 5.0, y: 6.0, lines: -3 }));
        }

        #[test]
        fn rejects_bad_input() {
            assert!(parse(&args("")).is_err());
            assert!(parse(&args("move 1")).is_err());
            assert!(parse(&args("move -1 2")).is_err());
            assert!(parse(&args("move nan 2")).is_err());
            assert!(parse(&args("click 1 2 sideways")).is_err());
            assert!(parse(&args("scroll 1 2 x")).is_err());
        }
    }
}
