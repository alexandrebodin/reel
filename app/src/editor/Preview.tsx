import { useEffect, useLayoutEffect, useRef, useState } from "react"

import { clamp, clickPlaced, zoomCentered } from "./edits"
import { player } from "./player"
import { beginDrag, dragTimeline, finishDrag, getState, useEditor } from "./store"

export function Preview() {
  const stage = useRef<HTMLDivElement>(null)
  const canvas = useRef<HTMLCanvasElement>(null)
  const exact = useRef<HTMLImageElement>(null)
  const dragging = useRef(false)
  const [room, setRoom] = useState({ w: 0, h: 0 })
  const project = useEditor((s) => s.project)
  const timeline = useEditor((s) => s.timeline)
  const selected = useEditor((s) => s.selected)
  const placing = selected?.kind === "zoom" || selected?.kind === "click"

  useLayoutEffect(() => {
    const observer = new ResizeObserver(([entry]) => setRoom({ w: entry.contentRect.width, h: entry.contentRect.height }))
    observer.observe(stage.current!)
    return () => observer.disconnect()
  }, [])

  useEffect(() => {
    player.attach(canvas.current!, exact.current!)
  }, [])

  useEffect(() => {
    player.refresh()
  }, [project, selected])

  useEffect(() => {
    player.draw()
  }, [timeline])

  const width = project?.layout.width ?? 16
  const height = project?.layout.height ?? 9
  const fit = Math.min(room.w / width, room.h / height) || 0

  const spot = (event: React.PointerEvent): [number, number] | null => {
    const { project } = getState()
    const view = player.view()
    if (!project || !view) return null
    const box = canvas.current!.getBoundingClientRect()
    const k = project.layout.width / box.width
    const scale = project.layout.w / view.w
    return [
      clamp(view.x + ((event.clientX - box.left) * k - project.layout.x) / scale, 0, project.size[0]),
      clamp(view.y + ((event.clientY - box.top) * k - project.layout.y) / scale, 0, project.size[1]),
    ]
  }

  const place = (event: React.PointerEvent) => {
    const { project, selected } = getState()
    const at = spot(event)
    if (!project || !selected || !at) return
    dragTimeline((timeline) =>
      selected.kind === "zoom"
        ? { ...timeline, zooms: timeline.zooms.map((z, i) => (i === selected.i ? zoomCentered(z, at[0], at[1], project.size) : z)) }
        : clickPlaced(timeline, selected.i, at[0], at[1])
    )
    player.draw()
  }

  return (
    <div ref={stage} className="relative flex min-h-0 flex-1 items-center justify-center">
      <div className="relative overflow-hidden rounded-md shadow-2xl ring-1 ring-border" style={{ width: width * fit, height: height * fit }}>
        <canvas
          ref={canvas}
          aria-label="Preview of the video"
          className={placing ? "block size-full cursor-crosshair touch-none" : "block size-full"}
          onPointerDown={(event) => {
            if (!placing) return
            event.currentTarget.setPointerCapture(event.pointerId)
            dragging.current = true
            beginDrag()
            place(event)
          }}
          onPointerMove={(event) => {
            if (dragging.current) place(event)
          }}
          onPointerUp={() => {
            if (dragging.current) finishDrag()
            dragging.current = false
          }}
        />
        <img ref={exact} alt="" hidden className="pointer-events-none absolute inset-0 size-full" />
      </div>
    </div>
  )
}
