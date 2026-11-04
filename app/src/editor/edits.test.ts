import { describe, expect, it } from "vitest"

import { markRemoved, addClick, addSpeed, addZoom, clickAt, clickPlaced, clickRemoved, dragged, gapAround, newSpanAt, outputAt, timecode, zoomCentered, zoomRect } from "./edits"
import type { Timeline } from "./types"

const empty: Timeline = { zooms: [], speeds: [], moves: [], clicks: [], typing: [] }
const span = (start_ms: number, end_ms: number) => ({ start_ms, end_ms })
const FRAME: [number, number] = [1000, 500]

describe("finding room for a mark", () => {
  it("sees the gap between its neighbours", () => {
    expect(gapAround([span(1000, 2000), span(5000, 6000)], 3000, 10000)).toEqual([2000, 5000])
  })

  it("has no gap inside a mark", () => {
    const [from, to] = gapAround([span(1000, 2000)], 1500, 10000)
    expect(from).toBe(to)
  })

  it("starts a new mark at the time asked for", () => {
    expect(newSpanAt([], 3000, 10000)).toEqual(span(3000, 5000))
  })

  it("slides a new mark back to fit before the next one", () => {
    expect(newSpanAt([span(4000, 5000)], 3500, 10000)).toEqual(span(2000, 4000))
  })

  it("makes do with a short gap but refuses one too small", () => {
    expect(newSpanAt([span(0, 1000), span(1200, 2000)], 1100, 10000)).toEqual(span(1000, 1200))
    expect(newSpanAt([span(0, 1000), span(1199, 2000)], 1100, 10000)).toBeNull()
    expect(newSpanAt([span(0, 1000)], 500, 10000)).toBeNull()
  })
})

describe("adding marks", () => {
  it("keeps zooms in time order and reports where the new one went", () => {
    const first = addZoom(empty, 5000, 10000, 100, 100, 1.4)!
    const second = addZoom(first.timeline, 1000, 10000, 200, 200, 2)!
    expect(second.index).toBe(0)
    expect(second.timeline.zooms.map((z) => z.start_ms)).toEqual([1000, 5000])
    expect(second.timeline.zooms[0]).toMatchObject({ x: 200, y: 200, level: 2 })
  })

  it("starts a speed mark at double speed", () => {
    expect(addSpeed(empty, 1000, 10000)!.timeline.speeds[0].speed).toBe(2)
  })

  it("does not touch the timeline it was given", () => {
    addZoom(empty, 1000, 10000, 0, 0, 2)
    expect(empty.zooms).toEqual([])
  })
})

describe("dragging a mark", () => {
  const spans = [span(1000, 2000), span(3000, 4000), span(6000, 7000)]

  it("moves the whole mark by its body", () => {
    expect(dragged(spans, 1, "body", spans[1], 500, 10000)[1]).toEqual(span(3500, 4500))
  })

  it("stops the body at its neighbours and at the ends of the take", () => {
    expect(dragged(spans, 1, "body", spans[1], -5000, 10000)[1]).toEqual(span(2000, 3000))
    expect(dragged(spans, 1, "body", spans[1], 5000, 10000)[1]).toEqual(span(5000, 6000))
    expect(dragged(spans, 0, "body", spans[0], -5000, 10000)[0]).toEqual(span(0, 1000))
    expect(dragged(spans, 2, "body", spans[2], 9000, 10000)[2]).toEqual(span(9000, 10000))
  })

  it("moves only the edge it was grabbed by, and lets it touch a neighbour", () => {
    expect(dragged(spans, 1, "start", spans[1], -5000, 10000)[1]).toEqual(span(2000, 4000))
    expect(dragged(spans, 1, "end", spans[1], 5000, 10000)[1]).toEqual(span(3000, 6000))
  })

  it("never lets a mark shrink below the minimum", () => {
    expect(dragged(spans, 1, "start", spans[1], 5000, 10000)[1]).toEqual(span(3800, 4000))
    expect(dragged(spans, 1, "end", spans[1], -5000, 10000)[1]).toEqual(span(3000, 3200))
  })

  it("measures from where the drag began, not from the last step", () => {
    const once = dragged(spans, 1, "body", spans[1], 100, 10000)
    expect(dragged(once, 1, "body", spans[1], 200, 10000)[1]).toEqual(span(3200, 4200))
  })
})

describe("clicks and the cursor", () => {
  it("brings the cursor to a new click just before it", () => {
    const { timeline } = addClick(empty, 2000, 50, 60, true)
    expect(timeline.moves).toEqual([{ start_ms: 1230, end_ms: 1880, x: 50, y: 60, pointer: "arrow" }])
  })

  it("adds no travel for a video that has its own cursor", () => {
    expect(addClick(empty, 2000, 50, 60, false).timeline.moves).toEqual([])
  })

  it("starts the travel after the previous one ends", () => {
    const first = addClick(empty, 2000, 50, 60, true).timeline
    const { timeline } = addClick(first, 2200, 70, 80, true)
    expect(timeline.moves[1].start_ms).toBe(1880)
    expect(timeline.moves[1].end_ms).toBe(2080)
  })

  it("moves the travel along with the click, keeping it on the click", () => {
    const { timeline } = addClick(empty, 2000, 50, 60, true)
    const moved = clickAt(timeline, 0, 3000).timeline
    expect(moved.moves[0]).toMatchObject({ start_ms: 2230, end_ms: 2880 })
    expect(clickAt(moved, 0, 4000).timeline.moves[0].end_ms).toBe(3880)
  })

  it("follows a click past another one", () => {
    const a = addClick(empty, 2000, 1, 1, true).timeline
    const b = addClick(a, 5000, 2, 2, true).timeline
    const { timeline, index } = clickAt(b, 0, 6000)
    expect(index).toBe(1)
    expect(timeline.clicks.map((c) => c.x)).toEqual([2, 1])
  })

  it("places the cursor's travel where the click is placed", () => {
    const { timeline } = addClick(empty, 2000, 50, 60, true)
    expect(clickPlaced(timeline, 0, 7, 8).moves[0]).toMatchObject({ x: 7, y: 8 })
    expect(clickPlaced(timeline, 0, 7, 8).clicks[0]).toMatchObject({ x: 7, y: 8 })
  })

  it("removes the travel with the click", () => {
    const { timeline } = addClick(empty, 2000, 50, 60, true)
    expect(clickRemoved(timeline, 0)).toMatchObject({ clicks: [], moves: [] })
  })

  it("keeps a travel another click at the same moment uses", () => {
    const one = addClick(empty, 2000, 50, 60, true).timeline
    const both = { ...one, clicks: [...one.clicks, { at_ms: 2000, x: 50, y: 60 }] }
    expect(clickRemoved(both, 0).moves.length).toBe(1)
  })
})

describe("removing a mark", () => {
  const timeline = addSpeed(addZoom(addClick(empty, 2000, 1, 1, true).timeline, 0, 10000, 0, 0, 2)!.timeline, 0, 10000)!.timeline

  it("removes only the selected kind", () => {
    expect(markRemoved(timeline, { kind: "zoom", i: 0 })).toMatchObject({ zooms: [], clicks: [{ at_ms: 2000 }] })
    expect(markRemoved(timeline, { kind: "speed", i: 0 }).speeds).toEqual([])
    expect(markRemoved(timeline, { kind: "click", i: 0 })).toMatchObject({ clicks: [], moves: [] })
    expect(markRemoved(timeline, { kind: "click", i: 0 }).zooms.length).toBe(1)
  })
})

describe("what a zoom shows", () => {
  it("is centred on its spot", () => {
    expect(zoomRect({ x: 500, y: 250, level: 2 }, FRAME)).toEqual({ x: 250, y: 125, w: 500, h: 250 })
  })

  it("stays inside the frame", () => {
    expect(zoomRect({ x: 0, y: 500, level: 2 }, FRAME)).toEqual({ x: 0, y: 250, w: 500, h: 250 })
  })

  it("keeps a dragged centre where the camera can look", () => {
    const zoom = { start_ms: 0, end_ms: 1, x: 500, y: 250, level: 2 }
    expect(zoomCentered(zoom, 0, 0, FRAME)).toMatchObject({ x: 250, y: 125 })
  })
})

describe("the finished video's clock", () => {
  const speeds = [
    { start_ms: 1000, end_ms: 3000, speed: 2 },
    { start_ms: 4000, end_ms: 6000, speed: null },
  ]

  it("runs fast through a speed change and skips a cut", () => {
    expect(outputAt(speeds, 500)).toBe(500)
    expect(outputAt(speeds, 2000)).toBe(1500)
    expect(outputAt(speeds, 3500)).toBe(2500)
    expect(outputAt(speeds, 5000)).toBe(3000)
    expect(outputAt(speeds, 7000)).toBe(4000)
  })

  it("reads as minutes and seconds", () => {
    expect(timecode(0)).toBe("00:00.00")
    expect(timecode(61500)).toBe("01:01.50")
  })
})
