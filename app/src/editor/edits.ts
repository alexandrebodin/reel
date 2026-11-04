import type { Click, Selection, SpeedSpan, Timeline, ZoomSpan } from "./types"

export const MIN_MARK_MS = 200
export const NEW_MARK_MS = 2000
const ARRIVES_EARLY_MS = 120
const TRAVEL_MS = 650
const MIN_TRAVEL_MS = 80

type Span = { start_ms: number; end_ms: number }

/** The free stretch around a time on a track, or an empty one if the time is inside a mark. */
export function gapAround(spans: Span[], ms: number, duration: number): [number, number] {
  let [from, to] = [0, duration]
  for (const span of spans) {
    if (span.end_ms <= ms) from = Math.max(from, span.end_ms)
    else if (span.start_ms >= ms) to = Math.min(to, span.start_ms)
    else return [span.end_ms, span.end_ms]
  }
  return [from, to]
}

/** Where a new mark goes at a time: `null` when there is no room for one. */
export function newSpanAt(spans: Span[], ms: number, duration: number): Span | null {
  const [from, to] = gapAround(spans, ms, duration)
  if (to - from < MIN_MARK_MS) return null
  const start_ms = Math.max(from, Math.min(ms, to - NEW_MARK_MS))
  return { start_ms, end_ms: Math.min(to, start_ms + NEW_MARK_MS) }
}

function inserted<T extends Span>(spans: T[], span: T) {
  const next = [...spans, span].sort((a, b) => a.start_ms - b.start_ms)
  return { spans: next, index: next.indexOf(span) }
}

export function addZoom(timeline: Timeline, ms: number, duration: number, x: number, y: number, level: number) {
  const span = newSpanAt(timeline.zooms, ms, duration)
  if (!span) return null
  const { spans, index } = inserted<ZoomSpan>(timeline.zooms, { ...span, x, y, level })
  return { timeline: { ...timeline, zooms: spans }, index }
}

export function addSpeed(timeline: Timeline, ms: number, duration: number) {
  const span = newSpanAt(timeline.speeds, ms, duration)
  if (!span) return null
  const { spans, index } = inserted<SpeedSpan>(timeline.speeds, { ...span, speed: 2 })
  return { timeline: { ...timeline, speeds: spans }, index }
}

export type SpanPart = "start" | "end" | "body"

/** Applies a drag to the mark as it was when the drag began. It stops at its neighbours and at the ends of the take. */
export function dragged<T extends Span>(spans: T[], i: number, part: SpanPart, origin: Span, by: number, duration: number): T[] {
  const floor = i > 0 ? spans[i - 1].end_ms : 0
  const ceiling = i < spans.length - 1 ? spans[i + 1].start_ms : duration
  const span = { ...spans[i] }
  if (part === "start") {
    span.start_ms = Math.max(floor, Math.min(span.end_ms - MIN_MARK_MS, origin.start_ms + by))
  } else if (part === "end") {
    span.end_ms = Math.min(ceiling, Math.max(span.start_ms + MIN_MARK_MS, origin.end_ms + by))
  } else {
    const length = origin.end_ms - origin.start_ms
    span.start_ms = Math.max(floor, Math.min(ceiling - length, origin.start_ms + by))
    span.end_ms = span.start_ms + length
  }
  return spans.map((s, at) => (at === i ? span : s))
}

export function isSameClickTravel(click: Click, moveEnd: number) {
  return Math.abs(moveEnd - (click.at_ms - ARRIVES_EARLY_MS)) < 1
}

/** A click on a drawn cursor brings the cursor along. A video with its own cursor has none to bring. */
export function addClick(timeline: Timeline, ms: number, x: number, y: number, withMove: boolean) {
  const click: Click = { at_ms: ms, x, y }
  const clicks = [...timeline.clicks, click].sort((a, b) => a.at_ms - b.at_ms)
  const end_ms = ms - ARRIVES_EARLY_MS
  let moves = timeline.moves
  if (withMove && end_ms > 0) {
    const before = moves.filter((m) => m.end_ms < end_ms).reduce((latest, m) => Math.max(latest, m.end_ms), 0)
    const start_ms = Math.min(end_ms - MIN_TRAVEL_MS, Math.max(before, end_ms - TRAVEL_MS))
    if (start_ms >= 0) {
      moves = [...moves, { start_ms, end_ms, x, y, pointer: "arrow" as const }].sort((a, b) => a.end_ms - b.end_ms)
    }
  }
  return { timeline: { ...timeline, clicks, moves }, index: clicks.indexOf(click) }
}

/** Moves a click in time, along with the cursor's travel to it. */
export function clickAt(timeline: Timeline, i: number, at_ms: number) {
  const click = timeline.clicks[i]
  const by = at_ms - click.at_ms
  const moves = timeline.moves.map((m) => {
    if (!isSameClickTravel(click, m.end_ms)) return m
    const end_ms = m.end_ms + by
    const start_ms = Math.max(0, m.start_ms + by)
    return { ...m, end_ms, start_ms: start_ms >= end_ms ? Math.max(0, end_ms - MIN_TRAVEL_MS) : start_ms }
  })
  const moved = { ...click, at_ms }
  const clicks = timeline.clicks.map((c, at) => (at === i ? moved : c)).sort((a, b) => a.at_ms - b.at_ms)
  return { timeline: { ...timeline, clicks, moves }, index: clicks.indexOf(moved) }
}

export function clickPlaced(timeline: Timeline, i: number, x: number, y: number): Timeline {
  const click = timeline.clicks[i]
  return {
    ...timeline,
    clicks: timeline.clicks.map((c, at) => (at === i ? { ...c, x, y } : c)),
    moves: timeline.moves.map((m) => (isSameClickTravel(click, m.end_ms) ? { ...m, x, y } : m)),
  }
}

/** Removes a click, and the cursor's travel to it unless another click uses that travel too. */
export function clickRemoved(timeline: Timeline, i: number): Timeline {
  const click = timeline.clicks[i]
  const shared = timeline.clicks.some((c, at) => at !== i && Math.abs(c.at_ms - click.at_ms) < 1)
  return {
    ...timeline,
    clicks: timeline.clicks.filter((_, at) => at !== i),
    moves: shared ? timeline.moves : timeline.moves.filter((m) => !isSameClickTravel(click, m.end_ms)),
  }
}

export function markRemoved(timeline: Timeline, selected: NonNullable<Selection>): Timeline {
  if (selected.kind === "zoom") return { ...timeline, zooms: timeline.zooms.filter((_, i) => i !== selected.i) }
  if (selected.kind === "speed") return { ...timeline, speeds: timeline.speeds.filter((_, i) => i !== selected.i) }
  return clickRemoved(timeline, selected.i)
}

/** The part of the frame a zoom shows, kept inside the frame. */
export function zoomRect(zoom: Pick<ZoomSpan, "x" | "y" | "level">, [W, H]: [number, number]) {
  const [w, h] = [W / zoom.level, H / zoom.level]
  return { x: clamp(zoom.x - w / 2, 0, W - w), y: clamp(zoom.y - h / 2, 0, H - h), w, h }
}

/** A zoom's centre, moved to where the camera can actually look. */
export function zoomCentered(zoom: ZoomSpan, x: number, y: number, frame: [number, number]): ZoomSpan {
  const r = zoomRect({ ...zoom, x, y }, frame)
  return { ...zoom, x: r.x + r.w / 2, y: r.y + r.h / 2 }
}

/** Where a time of the take lands in the finished video. */
export function outputAt(speeds: SpeedSpan[], ms: number) {
  let removed = 0
  for (const span of speeds) {
    if (ms <= span.start_ms) break
    const inside = Math.min(ms, span.end_ms) - span.start_ms
    removed += span.speed === null ? inside : inside - inside / span.speed
  }
  return ms - removed
}

export function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value))
}

export function timecode(ms: number) {
  const seconds = Math.max(0, ms) / 1000
  const minutes = Math.floor(seconds / 60)
  return `${String(minutes).padStart(2, "0")}:${(seconds - minutes * 60).toFixed(2).padStart(5, "0")}`
}
