use super::*;
use oink_foundation::Point;

fn display(key: &str, x: f64, y: f64, primary: bool, mirror: Option<&str>) -> NativeDisplay {
    NativeDisplay {
        key: key.into(),
        bounds: Rect::new(x, y, 100.0, 100.0),
        primary,
        mirror_of: mirror.map(str::to_string),
        mode: format!("mode-{key}"),
    }
}

fn handle(crtc: RRCrtc, outputs: &[RROutput]) -> Handle {
    Handle {
        crtc,
        mode: 7,
        rotation: 1,
        outputs: outputs.to_vec(),
    }
}

fn current() -> Snapshot {
    Snapshot {
        config: NativeConfiguration {
            displays: vec![
                display("a", 0.0, 0.0, true, None),
                display("b", 100.0, 0.0, false, None),
                display("b2", 100.0, 0.0, false, Some("b")),
            ],
        },
        handles: BTreeMap::from([
            ("a".to_string(), handle(10, &[1])),
            ("b".to_string(), handle(11, &[2, 3])),
        ]),
    }
}

fn origin(key: &str, x: f64, y: f64) -> NativeOrigin {
    NativeOrigin {
        key: key.into(),
        origin: Point::new(x, y),
    }
}

#[test]
fn moves_only_moved_sources_and_never_writes_mirror_followers() {
    let current = current();
    let unchanged = current
        .config
        .with_origins(&[origin("a", 0.0, 0.0), origin("b", 100.0, 0.0)])
        .unwrap();
    assert!(plan(&current, &unchanged).unwrap().is_empty());

    let target = current
        .config
        .with_origins(&[origin("a", 0.0, 0.0), origin("b", 0.0, 100.0)])
        .unwrap();
    let moves = plan(&current, &target).unwrap();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].key, "b");
    assert_eq!(moves[0].from, (100, 0));
    assert_eq!(moves[0].to, (0, 100));
    assert_eq!(moves[0].handle, handle(11, &[2, 3]));
}

#[derive(Default)]
struct Fake {
    calls: Vec<String>,
    fail_at: Option<usize>,
}

impl Writes for Fake {
    fn set_origin(&mut self, handle: &Handle, x: i32, y: i32) -> Result<(), DisplayError> {
        let at = self.calls.len();
        self.calls.push(format!("set:{}:{x}:{y}", handle.crtc));
        if self.fail_at == Some(at) {
            Err(DisplayError::Platform("fixture".into()))
        } else {
            Ok(())
        }
    }

    fn sync(&mut self) {
        self.calls.push("sync".into());
    }
}

fn moves() -> Vec<Move> {
    vec![
        Move {
            key: "a".into(),
            from: (0, 0),
            to: (0, 10),
            handle: handle(10, &[1]),
        },
        Move {
            key: "b".into(),
            from: (100, 0),
            to: (0, 100),
            handle: handle(11, &[2, 3]),
        },
    ]
}

#[test]
fn commit_writes_each_move_then_syncs_once() {
    let mut writer = Fake::default();
    commit(&mut writer, &moves()).unwrap();
    assert_eq!(writer.calls, ["set:10:0:10", "set:11:0:100", "sync"]);
}

#[test]
fn a_failed_step_rolls_back_the_applied_prefix_in_reverse() {
    let mut writer = Fake {
        fail_at: Some(1),
        ..Default::default()
    };
    assert!(commit(&mut writer, &moves()).is_err());
    assert_eq!(writer.calls, ["set:10:0:10", "set:11:0:100", "set:10:0:0", "sync"]);
}

#[test]
fn verification_mismatch_rolls_back_every_move_in_reverse() {
    let moves = moves();
    let all: Vec<&Move> = moves.iter().collect();
    let mut writer = Fake::default();
    rollback(&mut writer, &all).unwrap();
    assert_eq!(writer.calls, ["set:11:100:0", "set:10:0:0", "sync"]);
}
