# bevy_symbios_avatar

Bevy integration for [`symbios-avatar`](https://crates.io/crates/symbios-avatar),
which builds parametric 3D avatars from atproto records.

This crate draws them. It decides nothing about what a body looks like — every
shape, weight, chart and colour comes from `Avatar::build` — and it holds no
opinion about the scene a body stands in.

## Why it exists

The engine ships with a software renderer, and every quality judgement made
about a body so far has been made through it. Twice that instrument produced a
false diagnosis on its own: a blink that looked broken because lids and globes
were drawn in one colour, and UV seam normals that read as a skinning defect and
sent a day's work after the wrong culprit.

A defect that appears in one renderer and not the other is a defect in the
renderer. One that appears in both is a defect in the body. There is no way to
tell those apart with one instrument.

It is also the only place the shipping tier is real. The budget the engine is
judged against — one to three skinned draws, thirty thousand triangles, a WebGL2
feature set — is a number in a document until something uploads the meshes to a
GPU and a frame comes back.

## Installing

```toml
[dependencies]
bevy = "0.18"
bevy_symbios_avatar = "0.2"
symbios-avatar = "0.2"   # the record and archetype types you build bodies from
```

### Features

| Feature | Default | What it carries |
| --- | --- | --- |
| `editor` | **on** | The record editor and the motion window, and with them the only dependencies this crate has beyond Bevy and the engine — `bevy_egui` and `serde_json`. `default-features = false` removes all three; driving a body needs no GUI and is always compiled. |
| `builtin-clips` | off | The engine's baked CC0 clip set, embedded in the binary. Off because the artifact is 200 KiB and a consumer that only draws bodies should not pay for motion it never plays — least of all a wasm one. A consumer that fetches `clips.bin` at run time inserts its own `Clips` resource and pays nothing here. |

## Drawing a body

```rust,no_run
use bevy::prelude::*;
use bevy_symbios_avatar::{AvatarPlugin, SpawnAvatar};
use symbios_avatar::{Archetype, AvatarRecord};

App::new()
    .add_plugins((DefaultPlugins, AvatarPlugin))
    .add_systems(Startup, |mut commands: Commands| {
        commands.spawn(SpawnAvatar::from(AvatarRecord::new(
            "Someone",
            Archetype::default(),
        )));
    })
    .run();
```

A body becomes a root entity, one entity per joint of the rig hanging off it in
the rig's own hierarchy, and one entity per drawn mesh — which is also the shape
the draw budget is stated in, one draw per merged mesh. Write an `AvatarPose` on
the root and the joints follow; `AvatarJoints` indexes the joint entities in the
rig's own order, and `AvatarBody` keeps the whole built `Avatar` for anything
that wants to ask what it cost or where its head is.

Bring your own camera and lights. `AvatarPlugin` draws a body and nothing else.

### The order a frame does things in

`AvatarSystems` declares three sets, chained in `Update`: `Build`, then
`Animate`, then `Apply`. A body is **destroyed and rebuilt** whenever its record
changes — that is what a re-roll is, and what every step of a slider in the
editor is — so anything that decides what to do with a body has to run after the
set that could have removed it. Systems of your own that touch avatars belong in
these sets rather than in bare `Update`.

## Moving a body

`AnimatorPlugin` adds an `Animator` resource, ticks the engine's motion every
frame and writes the result onto components. Everything that decides how a body
*moves* comes from the engine's own `anim` module, so a walk that reads wrong
here and right in the software renderer is this crate's fault, and one that reads
wrong in both is the engine's.

```rust,no_run
use bevy::prelude::*;
use bevy_symbios_avatar::{Animator, AnimatorPlugin, AvatarPlugin, SpawnAvatar};
use symbios_avatar::{Archetype, AvatarRecord};

App::new()
    .add_plugins((DefaultPlugins, AvatarPlugin, AnimatorPlugin))
    .add_systems(Startup, |mut commands: Commands, mut animator: ResMut<Animator>| {
        animator.walking = true;
        commands.spawn(SpawnAvatar::from(AvatarRecord::new(
            "Someone",
            Archetype::default(),
        )));
    })
    .run();
```

The resource is the whole control surface: the gait pattern and its cadence,
pace and phase; a swim or a leap instead of the walk and a gesture over it;
baked clips, layered or not; the ground's grade and camber, a turn rate and a
travel heading; blinking, a held lid closure, speech, a held jaw angle, a resting
expression and a held viseme; where the gaze is aimed and how far the chain may
turn; and, as readouts, how far the footing solve had to move the feet and how
many contacts it could not reach.

The two ground controls are two axes rather than one because a plane has two:
**grade** is the hill the body walks up or down, along `+z` where it faces, and
**camber** is the one it stands across, along `+x`. Together they reach every
plane through the origin, and each asks a gait a different question — a grade is
answered by stride and crouch, a camber by the ankles and the width of the
stance. `floor_tilt` and `ground_normal` are the one shared definition of that
plane, so a floor drawn from them cannot drift from the ground the feet are
solved against.

`cycle` and `scrub` are what earn the window: together they hold the gait at one
point in its cycle, which is the only way to actually look at a foot plant. A
gait judged at whatever phase a capture landed on is a gait judged at one pose.

Clips come from the `Clips` resource, which `AnimatorPlugin` fills from the
`builtin-clips` feature. Empty is a legitimate state: with no clips the procedural
gait is all there is, and the window says so. A clip can replace the gait or ride
over it — legs from the engine, arms from the library — with a blend between
sources and the clip's root travel taken out so the two stay comparable.

## Editing a record

`RecordEditorPlugin` (the `editor` feature) adds a panel holding every axis an
`AvatarRecord` can carry: 115 of them plus one whole-number count, across
archetype, composites, skin, eyes, face, five hair regions, outfit, name, seed
and the per-category locks a re-roll honours. Drag one and the body follows. A
test pins the exact count, so an axis added to the record cannot quietly miss the
panel.

It is not a debug panel, and two rules are what keep it from becoming one.

**The record, and only the record.** No slider touches an engine constant. A body
tuned against something a record cannot hold could not be saved, shared or
rebuilt by anyone else, and an afternoon spent perfecting one would produce
nothing anybody could keep. `copy` puts the record on the clipboard as JSON and
`load` reads one back. A share code does the same for a *look* alone — archetype,
composites and complexion at a byte an axis, keeping the name, seed and locks of
the record it lands in. Every value is quantised the way the wire format is —
scaled integers in thousandths, no floats — so what a slider shows is what a
record would hold.

**A judgement image is never a screenshot of a UI.** The panel hides on a key, it
hides for the frame a capture takes, and it never opens at all under the viewer's
`--shot`.

A rebuild costs 68 ms at a draft atlas and 277 at the full one, so an axis being
dragged rebuilds at the draft size — about fourteen frames a second — and the
full-size build lands a quarter of a second after it stops, on the compute pool
rather than on the frame. With the panel open and nothing moving it costs 0.1 ms
a frame.

The panel is composed from public per-section functions — `identity`,
`composite_axes`, `body_axes`, `skin_axes`, `eye_axes`, `face_axes`, `hair_axes`,
`outfit_axes` — and those, not the panel, are the reuse surface. A host with its
own window, theme, undo and rebuild pipeline calls the sections against its own
record and owes this module nothing else. See the [`editor`] module docs for the
contract.

Both windows draw in `bevy_egui`'s `EguiPrimaryContextPass`, so an app that wants
them adds `EguiPlugin` alongside these plugins, as the viewer does.

[`editor`]: https://docs.rs/bevy_symbios_avatar/latest/bevy_symbios_avatar/editor/

## The viewer

```text
cargo run --release -F builtin-clips --example viewer
cargo run --release -F builtin-clips --example viewer -- --seed 7
cargo run --release -F builtin-clips --example viewer -- --quadruped
cargo run --release -F builtin-clips --example viewer -- --shot body.png
```

The example requires `builtin-clips` — its clip picker is not a picker with
nothing to pick — and the flag is spelled out because the feature is off by
default. The full flag set — gaits, clips, face framing, held closures, captures
— is documented at the top of [`examples/viewer.rs`](examples/viewer.rs).

Right-drag orbits, middle-drag pans, the wheel zooms. `W` walks, `Space`
re-rolls, `H` hides the windows, `F` frames the camera on the body again, `P`
saves a picture, `B` prints what the body costs. Run it in release: building a
body subdivides, binds, unwraps and paints a megapixel atlas.

Editing a record never moves the camera. A body is destroyed and rebuilt on every
step of a slider, so anything that re-framed on a new body would be re-framing on
every edit — throwing away the pan and zoom several times a second while an axis
is being dragged. `F` is the re-frame, and it takes a keypress on purpose.

Left-drag is deliberately not a camera control. It belongs to the GUI, and a
camera that also answered to it would fight every slider on the screen. What is
left to arbitrate is handled by a gate that blocks camera input while a window
wants the pointer, with one exception: a **held** right or middle button always
drives the camera, so an orbit never dies because the drag crossed a window. (The
camera crate ships a `bevy_egui` feature that would do this; it is off on purpose
— its gate is all-or-nothing and kills exactly that case.)

## Versions

Bevy 0.18, `bevy_egui` 0.39, `symbios-avatar` 0.1 — the set known to work
together, rather than whatever is latest. Rust edition 2024.

## Licence

MIT.
