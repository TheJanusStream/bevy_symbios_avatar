# bevy_symbios_avatar

Bevy integration for [`symbios-avatar`](https://github.com/TheJanusStream/symbios-avatar),
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

## Using it

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
the draw budget is stated in. Write an `AvatarPose` on the root and the joints
follow.

## The viewer

```text
cargo run --release --features builtin-clips --example viewer
cargo run --release --features builtin-clips --example viewer -- --seed 7
cargo run --release --features builtin-clips --example viewer -- --quadruped
cargo run --release --features builtin-clips --example viewer -- --shot body.png
```

The example requires `builtin-clips` — its clip picker is not a picker with
nothing to pick — and the flag is spelled out because the feature is off by
default. The full flag set — gaits, clips, face framing, held closures, captures
— is documented at the top of [`examples/viewer.rs`](examples/viewer.rs).

Right-drag orbits, middle-drag pans, the wheel zooms — the same bindings as the
sibling application, so the hands that use one do not have to relearn the other.
`W` walks, `Space` re-rolls, `H` hides the windows, `F` frames the camera on the
body again, `P` saves a picture, `B` prints what the body costs. Run it in
release: building a body subdivides, binds, unwraps and paints a megapixel
atlas.

Editing a record never moves the camera. A body is destroyed and rebuilt on
every step of a slider, so anything that re-framed on a new body would be
re-framing on every edit — throwing away the pan and zoom several times a second
while an axis is being dragged. `F` is the re-frame, and it takes a keypress on
purpose.

Left-drag is deliberately not a camera control. It belongs to the GUI, and a
camera that also answered to it would fight every slider on the screen. What is
left to arbitrate is handled by a gate that blocks camera input while a window
wants the pointer, with one exception: a **held** right or middle button always
drives the camera, so an orbit never dies because the drag crossed a window.
(The camera crate ships a `bevy_egui` feature that would do this; it is off on
purpose — its gate is all-or-nothing and kills exactly that case.)

## The record editor

A panel holding every axis an `AvatarRecord` can carry — over a hundred of
them, across archetype, composites, skin, eyes, face, five hair regions,
outfit, name, seed and the per-category locks a re-roll honours. Drag one and
the body follows. A test pins the exact count, so an axis added to the record
cannot quietly miss the panel.

It is not a debug panel, and two rules are what keep it from becoming one.

**The record, and only the record.** No slider touches an engine constant. A
body tuned against something a record cannot hold could not be saved, shared or
rebuilt by anyone else, and an afternoon spent perfecting one would produce
nothing anybody could keep. `copy` puts the record on the clipboard as JSON and
`load` reads one back, so a body somebody was fiddling with becomes a body
anybody can rebuild. A share code does the same for a *look* alone — archetype,
composites and complexion at a byte an axis, keeping the name, seed and locks
of the record it lands in. Every value is quantised the way the wire format is
— scaled integers in thousandths, no floats — so what a slider shows is what a
record would hold.

**A judgement image is never a screenshot of a UI.** `H` hides the panel, `P`
hides it for the frame it captures, and `--shot` never opens it at all.

A rebuild costs 68 ms at a draft atlas and 277 at the full one, so an axis
being dragged rebuilds at the draft size — about fourteen frames a second — and
the full-size build lands a quarter of a second after it stops. With the panel
open and nothing moving it costs 0.1 ms a frame.

`default-features = false` removes it, and with it the only dependencies this
crate has beyond Bevy and the engine — `bevy_egui` and `serde_json`.

## The motion window

The second window, for what a body is *doing* rather than what it is: walk on
or off, which gait pattern, cadence, pace, arm swing, foot planting, a ground
slope, blinking, a held lid closure, speech, a held jaw angle, and where the
gaze is aimed. All of it comes from the engine's own `anim` module — this crate
ticks a cycle and writes the result onto components — so a walk that reads
wrong here and right in the software renderer is this crate's fault, and one
that reads wrong in both is the engine's.

It also plays the engine's baked clips: pick one and it replaces the gait, or
rides over it so a baked gesture can top a procedural walk, with a blend
between sources and the clip's root travel taken out so the two stay
comparable. The window reports how far the footing solve had to move the feet,
which is the number a locomotion comparison should be settled on. The clip set
is behind the `builtin-clips` feature — 200 KiB a wasm build should fetch
rather than carry — and a consumer that fetches `clips.bin` at run time inserts
its own `Clips` resource instead.

`scrub` is the control that earns the window: it holds the gait at one point in
its cycle, which is the only way to actually look at a foot plant. A gait judged
at whatever phase a capture landed on is a gait judged at one pose.

Driving a body needs no GUI and is always compiled; only the window is behind
the `editor` feature.

## Versions

Bevy 0.18, `bevy_egui` 0.39 — the pair known to work together, rather than
whatever is latest. `symbios-avatar` is a path dependency for as long as the
engine is pre-release: if the two instruments ever read different versions of it
they stop being comparable, which is the one thing this crate is here to do.

## Licence

MIT.
