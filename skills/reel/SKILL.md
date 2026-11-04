---
name: reel
description: Record a real, polished demo video of a web app by driving it with agent-browser while reel captures it, then automatically adds zooms, a cursor, click effects and cuts the waiting. Use when the user asks to record, film or make a demo, walkthrough, product video, screencast or video of a feature or flow in a web app, or says "show this working in a video", or asks to re-record or restyle a reel demo. Also opens a recording or any video in the Reel editor with reel edit. Not for screenshots, and not for recording anything outside a browser.
license: MIT
---

# reel

You click through the real app yourself with `agent-browser`, and reel records it. The clicks are real, so the video is a real demo. `reel rec stop` produces the finished video: no manual editing needed.

Requires `reel`, `agent-browser` and `ffmpeg` on the PATH. Load the `agent-browser` skill if you need its commands.

## Record a demo

Pick a short session name for the demo, such as `domains`. The video lands in `out/<session>/` under the current folder.

### 1. Open the browser and rehearse

```bash
reel rec open --session domains http://localhost:3000/domains
```

Always open with `reel rec open`, not `agent-browser open`: it starts the browser at 2x, without which the video is blurry. Add `--size 1280x720` for another size; 1920x1080 is the default.

Sign in now if needed, or start signed in with `--state auth.json`, a file from `agent-browser state save`. Only its cookies are used. Nothing is recorded yet. Then walk the flow once with `agent-browser --session domains ...` to learn the page and spot surprises. Finally put the app back where the demo starts: undo what the rehearsal created, and go back to the first page.

### 2. Record

```bash
reel rec start --session domains
```

Now drive the app with ordinary commands:

```bash
agent-browser --session domains snapshot -i
agent-browser --session domains click @e1
reel rec type --session domains "#hostname" "shop.acme.com"
reel rec hold --session domains 2000
```

Take your time: thinking pauses are removed automatically.

- **Type with `reel rec type`**, not `agent-browser type` or `fill`, which put the text in all at once. The exception is secrets: never type a real password in a take. Sign in during step 1.
- **Use `reel rec hold <ms>` after anything the viewer should read**, such as a result or a confirmation. Waiting without it gets trimmed to about a second.
- **Wait for the app before moving on**, as you normally would. A step the viewer can't follow looks like a glitch.
- **Stay in one tab.** Only the tab that was showing at `rec start` is recorded.

### 3. Fix mistakes as you go

If an action goes wrong, undo it in the app (close the dialog, go back), then take it out of the video:

```bash
reel rec cut --session domains --last 2
```

`--last N` removes the last N actions and everything since, including the time spent recovering. A run of typing in one field counts as one action, and so does a `reel rec hold`. It prints how many it removed.

The app itself must end up back in the right state before you continue, because a cut only hides what the viewer sees. If that isn't possible, stop and record a new take.

### 4. Stop

```bash
reel rec stop --session domains
```

This takes roughly 3 seconds per second of finished video, so run it in the background if your shell has a time limit. It prints a JSON summary with `video`, `recording`, `replay` and `actions`, and `left_out_of_replay` if it held back a secret.

### 5. Check it, then hand it over

Never hand over a video you haven't looked at. Make a contact sheet and read it:

```bash
ffmpeg -v error -y -i out/domains/domains.mp4 -vf "fps=1,scale=640:-1,tile=4x4" -frames:v 1 out/domains/sheet.png
```

Check that each step is visible, the camera shows what matters, and no error or wrong state appears. If a take is bad, record it again. Otherwise give the user the video's path, what it shows, and this line for editing it themselves:

```bash
reel edit out/domains/recording.json
```

## Change a finished demo

| The user wants | Do this | Re-records? |
| --- | --- | --- |
| A different look | `reel --render out/domains/recording.json --style key=value` | No |
| To tweak zooms, speed or cuts by hand | `reel edit out/domains/recording.json` | No |
| The same flow after the app changed | `reel out/domains/replay.json` | Yes, fast and with no agent driving |
| A different flow | Record a new take | Yes |

`replay.json` needs the app in the same starting state, and it stops with an error if a step no longer matches the page. Fix that step in the file, or record a new take. It contains the text that was typed, so check it before sharing. It leaves out passwords, and a take that typed one reports that in `left_out_of_replay`. To replay a signed-in page, save the session with `agent-browser --session domains state save auth.json` and pass `--state auth.json` to `reel rec start`.

## Style options

Pass them to `reel rec start` or `reel --render` as `--style key=value`, repeated as needed. Only set what the user asked for: the defaults are good.

| Key | Values |
| --- | --- |
| `zoom.level` | 1 to 8, default 1.4. 1 never zooms |
| `zoom.animation` | `smooth`, `dynamic` |
| `zoom.calm` | `true`, the default for `rec`, zooms only for typing, dialogs and clicks you `hold` after. `false` zooms on every click |
| `motion_blur` | `off`, `low`, `medium`, `high`. Makes a render about 3 times slower or more |
| `cursor.show`, `cursor.adaptive`, `cursor.hide_when_idle`, `cursor.hide_when_typing` | `true`, `false` |
| `cursor.size` | 0.25 to 8 |
| `cursor.style` | `white_arrow`, `black_arrow`, `dot` |
| `cursor.motion` | `natural`, `smooth` |
| `clicks.press`, `clicks.ripple` | `true`, `false` |
| `clicks.color` | `#rrggbb` or a name such as `blue` |
| `clicks.style` | `filled`, `outline` |
| `background.fill` | `none`, `#rrggbb`, `#from,#to` for a gradient, or an image's path |
| `background.aspect` | `auto`, `16:9`, `1:1`, `9:16` or any `w:h` |
| `background.padding` | Percent, below 45 |
| `background.shadow` | 0 to 100 |
| `background.corners` | Percent of the short side, 0 to 50 |

## When something goes wrong

- **`is at 1x, which would record blurry`**: the session's browser was started without reel. Run `agent-browser --session <name> close`, then `reel rec open`.
- **`is already recording`**: a take is still running. Run `reel rec stop` first.
- **`is not recording`**: run `reel rec start`.
- **Empty or wrong video**: read `out/<session>/rec.log`.
- **A hand cursor resting on an empty spot** after the clicked element disappears is a known limitation.
