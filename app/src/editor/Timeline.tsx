import { MousePointerClickIcon, PlusIcon, TimerIcon, ZoomInIcon, type LucideIcon } from "lucide-react"
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { cn } from "@/lib/utils"

import { addClick, addSpeed, addZoom, clamp, clickAt, dragged, timecode, type SpanPart } from "./edits"
import { player } from "./player"
import { beginDrag, dragTimeline, editTimeline, finishDrag, getState, select, useEditor } from "./store"

const EDGE_PX = 8

export function addMark(kind: "zoom" | "speed" | "click", ms: number) {
  const { project, timeline, style } = getState()
  if (!project || !timeline || !style) return
  const [W, H] = project.size
  const added =
    kind === "zoom"
      ? addZoom(timeline, ms, project.duration_ms, W / 2, H / 2, Math.max(1.4, style.zoom.level))
      : kind === "speed"
        ? addSpeed(timeline, ms, project.duration_ms)
        : (() => {
            const [x, y] = project.imported ? [W / 2, H / 2] : player.cursorSpot()
            return addClick(timeline, ms, x, y, !project.imported)
          })()
  if (!added) {
    toast.error(`No room for a ${kind} here. Move the playhead to a free stretch.`)
    return
  }
  editTimeline(() => added.timeline, { kind, i: added.index })
}

type Drag = { kind: "zoom" | "speed"; i: number; part: SpanPart; from: number; origin: { start_ms: number; end_ms: number }; moved: boolean } | { kind: "click"; i: number; from: number; origin: { at_ms: number }; moved: boolean } | { kind: "seek" }

export function TimelinePanel() {
  const project = useEditor((s) => s.project)
  const timeline = useEditor((s) => s.timeline)
  const selected = useEditor((s) => s.selected)
  const lanes = useRef<HTMLDivElement>(null)
  const head = useRef<HTMLDivElement>(null)
  const drag = useRef<Drag | null>(null)
  const [width, setWidth] = useState(0)

  const loaded = project !== null
  useLayoutEffect(() => {
    if (!lanes.current) return
    const observer = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width))
    observer.observe(lanes.current)
    return () => observer.disconnect()
  }, [loaded])

  const duration = project?.duration_ms ?? 1
  const percent = (ms: number) => `${(ms / duration) * 100}%`

  useEffect(
    () =>
      player.onFrame(() => {
        if (head.current) head.current.style.left = percent(player.t)
      }),
    [duration]
  )

  const msAt = (clientX: number) => {
    const box = lanes.current!.getBoundingClientRect()
    return clamp(((clientX - box.left) / box.width) * duration, 0, duration)
  }

  const onPointerDown = (event: React.PointerEvent) => {
    if (event.button !== 0) return
    const target = event.target as HTMLElement
    const mark = target.closest<HTMLElement>("[data-mark]")
    event.currentTarget.setPointerCapture(event.pointerId)
    if (!mark) {
      if (target.closest("[data-seek]")) {
        drag.current = { kind: "seek" }
        player.seek(msAt(event.clientX))
      } else {
        select(null)
      }
      return
    }
    const kind = mark.dataset.mark as "zoom" | "speed" | "click"
    const i = Number(mark.dataset.i)
    const box = mark.getBoundingClientRect()
    const part: SpanPart = event.clientX - box.left < EDGE_PX ? "start" : box.right - event.clientX < EDGE_PX ? "end" : "body"
    const from = msAt(event.clientX)
    const { timeline } = getState()
    select({ kind, i })
    if (kind === "click") drag.current = { kind, i, from, origin: { at_ms: timeline!.clicks[i].at_ms }, moved: false }
    else {
      const span = (kind === "zoom" ? timeline!.zooms : timeline!.speeds)[i]
      drag.current = { kind, i, part, from, origin: { start_ms: span.start_ms, end_ms: span.end_ms }, moved: false }
    }
  }

  const onPointerMove = (event: React.PointerEvent) => {
    const current = drag.current
    if (!current) return
    const ms = msAt(event.clientX)
    if (current.kind === "seek") {
      player.seek(ms)
      return
    }
    const by = ms - current.from
    if (!current.moved) {
      if (Math.abs(by) < (duration / lanes.current!.clientWidth) * 3) return
      current.moved = true
      beginDrag()
    }
    if (current.kind === "click") {
      const moved = clickAt(getState().timeline!, current.i, clamp(current.origin.at_ms + by, 0, duration))
      current.i = moved.index
      dragTimeline(() => moved.timeline, { kind: "click", i: moved.index })
    } else if (current.kind === "zoom") {
      const { i, part, origin } = current
      dragTimeline((t) => ({ ...t, zooms: dragged(t.zooms, i, part, origin, by, duration) }))
    } else {
      const { i, part, origin } = current
      dragTimeline((t) => ({ ...t, speeds: dragged(t.speeds, i, part, origin, by, duration) }))
    }
  }

  const onPointerUp = () => {
    const current = drag.current
    if (current && current.kind !== "seek" && current.moved) finishDrag()
    drag.current = null
  }

  const step = [500, 1000, 2000, 5000, 10000, 30000, 60000].find((s) => (s / duration) * width >= 64) ?? 120000
  const ticks: number[] = []
  for (let ms = 0; ms < duration; ms += step) ticks.push(ms)

  if (!project || !timeline) return <div className="h-48 border-t" />

  return (
    <div className="flex shrink-0 border-t bg-card">
      <div className="flex w-28 shrink-0 flex-col border-r text-xs text-muted-foreground">
        <div className="h-7" />
        <div className="flex h-11 items-center px-3">Video</div>
        <LaneHead icon={ZoomInIcon} name="Zoom" hint="Add a zoom at the playhead" shortcut="Z" onAdd={() => addMark("zoom", player.t)} />
        <LaneHead icon={TimerIcon} name="Speed" hint="Add a speed change at the playhead" shortcut="S" onAdd={() => addMark("speed", player.t)} />
        <LaneHead icon={MousePointerClickIcon} name="Clicks" hint="Add a click at the playhead" shortcut="C" onAdd={() => addMark("click", player.t)} />
      </div>
      <div className="min-w-0 flex-1 px-3 pb-2">
        <div ref={lanes} className="relative touch-none" onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp}>
          <div data-seek className="relative h-7 cursor-ew-resize overflow-hidden font-mono text-[10px] text-muted-foreground">
            {ticks.map((ms) => (
              <span key={ms} className="absolute top-2 border-l pl-1" style={{ left: percent(ms) }}>
                {timecode(ms).replace(/\.00$/, "")}
              </span>
            ))}
          </div>
          <div data-seek className="h-11 cursor-ew-resize overflow-hidden rounded-md bg-muted bg-size-[100%_100%] ring-1 ring-border" style={{ backgroundImage: "url(/strip.jpg)" }} />
          <Lane onAdd={(ms) => addMark("zoom", ms)} msAt={msAt}>
            {timeline.zooms.map((z, i) => (
              <Clip key={i} kind="zoom" i={i} selected={selected?.kind === "zoom" && selected.i === i} left={percent(z.start_ms)} width={percent(Math.min(z.end_ms, duration) - z.start_ms)} label={`${z.level.toFixed(1)}×`} />
            ))}
          </Lane>
          <Lane onAdd={(ms) => addMark("speed", ms)} msAt={msAt}>
            {timeline.speeds.map((s, i) => (
              <Clip key={i} kind="speed" i={i} cut={s.speed === null} selected={selected?.kind === "speed" && selected.i === i} left={percent(s.start_ms)} width={percent(s.end_ms - s.start_ms)} label={s.speed === null ? "Cut" : `${+s.speed.toFixed(2)}×`} />
            ))}
          </Lane>
          <Lane onAdd={(ms) => addMark("click", ms)} msAt={msAt}>
            {timeline.clicks.map((c, i) => (
              <button
                key={i}
                data-mark="click"
                data-i={i}
                aria-label={`Click at ${timecode(c.at_ms)}`}
                aria-pressed={selected?.kind === "click" && selected.i === i}
                className="absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 cursor-grab rounded-full border-2 border-foreground bg-background outline-none hover:bg-muted-foreground focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-foreground aria-pressed:ring-2 aria-pressed:ring-ring"
                style={{ left: percent(c.at_ms) }}
              />
            ))}
          </Lane>
          <div ref={head} className="pointer-events-none absolute top-1 bottom-0 w-px bg-foreground" style={{ left: 0 }}>
            <div className="absolute -top-0.5 -left-1 size-2 rounded-full bg-foreground" />
          </div>
        </div>
      </div>
    </div>
  )
}

function LaneHead({ icon: Icon, name, hint, shortcut, onAdd }: { icon: LucideIcon; name: string; hint: string; shortcut: string; onAdd: () => void }) {
  return (
    <div className="mt-1 flex h-8 items-center gap-2 pr-1 pl-3">
      <Icon className="size-3.5" />
      <span className="flex-1">{name}</span>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button variant="ghost" size="icon-xs" aria-label={hint} onClick={onAdd}>
            <PlusIcon />
          </Button>
        </TooltipTrigger>
        <TooltipContent>
          {hint} ({shortcut})
        </TooltipContent>
      </Tooltip>
    </div>
  )
}

function Lane({ children, onAdd, msAt }: { children: React.ReactNode; onAdd: (ms: number) => void; msAt: (clientX: number) => number }) {
  return (
    <div
      className="relative mt-1 h-8 rounded-md bg-muted/50"
      onDoubleClick={(event) => {
        if (!(event.target as HTMLElement).closest("[data-mark]")) onAdd(msAt(event.clientX))
      }}
    >
      {children}
    </div>
  )
}

function Clip({ kind, i, left, width, label, selected, cut }: { kind: "zoom" | "speed"; i: number; left: string; width: string; label: string; selected: boolean; cut?: boolean }) {
  return (
    <button
      data-mark={kind}
      data-i={i}
      aria-pressed={selected}
      aria-label={`${kind} ${label}`}
      style={{ left, width }}
      className={cn(
        "group absolute inset-y-0.5 min-w-1 cursor-grab overflow-hidden rounded-sm border px-2 text-left text-xs font-medium whitespace-nowrap outline-none focus-visible:ring-2 focus-visible:ring-ring",
        kind === "zoom" ? "border-track-zoom/60 bg-track-zoom/25 text-track-zoom" : "border-track-speed/60 bg-track-speed/25 text-track-speed",
        cut && "bg-[repeating-linear-gradient(135deg,var(--track-speed)_0_1px,transparent_1px_6px)]",
        selected && (kind === "zoom" ? "border-track-zoom ring-1 ring-track-zoom" : "border-track-speed ring-1 ring-track-speed")
      )}
    >
      <span className="absolute inset-y-0 left-0 w-2 cursor-ew-resize" />
      {label}
      <span className="absolute inset-y-0 right-0 w-2 cursor-ew-resize" />
    </button>
  )
}
