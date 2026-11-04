import { zoomRect } from "./edits"
import { getState, setState } from "./store"
import type { Project, Style, Timeline } from "./types"

const ROW = 9
const RIPPLE_MS = 500
const POINTERS = ["arrow", "hand", "text"] as const
const GUIDE = "#f5b83d"

type Sample = { vx: number; vy: number; vw: number; cx: number; cy: number; opacity: number; pointer: number; size: number }

function sampleAt(project: Project, ms: number): Sample {
  const { rows, sample_ms } = project
  const last = rows.length / ROW - 1
  const at = Math.max(0, Math.min(last, ms / sample_ms))
  const i = Math.floor(at)
  const j = Math.min(last, i + 1)
  const mix = (k: number) => rows[i * ROW + k] + (rows[j * ROW + k] - rows[i * ROW + k]) * (at - i)
  return { vx: mix(0), vy: mix(1), vw: mix(2), cx: mix(3), cy: mix(4), opacity: mix(6), pointer: rows[i * ROW + 7], size: mix(8) }
}

/**
 * Plays the take on a canvas from numbers the server sampled, and swaps in the server's exact frame
 * when paused. Runs outside React: the playhead moves sixty times a second.
 */
class Player {
  readonly video = document.createElement("video")
  t = 0
  private canvas: HTMLCanvasElement | null = null
  private exact: HTMLImageElement | null = null
  private images = { background: new Image(), arrow: new Image(), hand: new Image(), text: new Image() }
  private imagesFor: string | null = null
  private frameListeners = new Set<() => void>()
  private exactTimer = 0
  private exactWanted = 0

  constructor() {
    this.video.src = "/video"
    this.video.muted = true
    this.video.playsInline = true
    this.video.preload = "auto"
    this.video.addEventListener("seeked", () => this.draw())
    this.video.addEventListener("loadeddata", () => this.draw())
    this.video.addEventListener("ended", () => {
      this.t = getState().project?.duration_ms ?? this.t
      this.setPlaying(false)
      this.draw()
    })
    for (const image of Object.values(this.images)) image.onload = () => this.draw()
  }

  attach(canvas: HTMLCanvasElement, exact: HTMLImageElement) {
    this.canvas = canvas
    this.exact = exact
    this.refresh()
  }

  onFrame(listener: () => void) {
    this.frameListeners.add(listener)
    return () => {
      this.frameListeners.delete(listener)
    }
  }

  /** Call when the project, the look or the selection changes. */
  refresh() {
    const { project } = getState()
    if (!project || !this.canvas) return
    const { width, height } = project.layout
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width
      this.canvas.height = height
    }
    const key = JSON.stringify(project.style)
    if (key !== this.imagesFor) {
      this.imagesFor = key
      this.images.background.src = `/background.png?g=${project.generation}`
      for (const pointer of POINTERS) this.images[pointer].src = `/sprite.png?pointer=${pointer}&g=${project.generation}`
    }
    this.draw()
    this.requestExact()
  }

  /** What the picture shows now: the whole frame while framing a zoom, so its frame can be dragged. */
  view() {
    const { project, selected } = getState()
    if (!project) return null
    const [W, H] = project.size
    if (selected?.kind === "zoom") return { x: 0, y: 0, w: W, h: H }
    const sample = sampleAt(project, this.t)
    return { x: sample.vx, y: sample.vy, w: sample.vw, h: (sample.vw * H) / W }
  }

  cursorSpot(): [number, number] {
    const { project } = getState()
    if (!project) return [0, 0]
    const sample = sampleAt(project, this.t)
    return [sample.cx, sample.cy]
  }

  draw() {
    const { project, style, timeline } = getState()
    const ctx = this.canvas?.getContext("2d")
    const view = this.view()
    if (!project || !style || !timeline || !ctx || !view || !this.images.background.naturalWidth) return
    const { layout, size } = project
    ctx.drawImage(this.images.background, 0, 0, layout.width, layout.height)
    ctx.save()
    ctx.beginPath()
    ctx.roundRect(layout.x, layout.y, layout.w, layout.h, layout.radius)
    ctx.clip()
    if (this.video.videoWidth) {
      const k = this.video.videoWidth / size[0]
      ctx.imageSmoothingQuality = "high"
      ctx.drawImage(this.video, view.x * k, view.y * k, view.w * k, view.h * k, layout.x, layout.y, layout.w, layout.h)
    }
    this.drawRipples(ctx, project, style, timeline, view)
    ctx.restore()
    this.drawCursor(ctx, project, style, view)
    this.drawGuides(ctx, project, timeline, view)
    this.frameListeners.forEach((listener) => listener())
  }

  private placed(project: Project, view: { x: number; y: number; w: number }, x: number, y: number) {
    const scale = project.layout.w / view.w
    return [project.layout.x + (x - view.x) * scale, project.layout.y + (y - view.y) * scale]
  }

  private drawRipples(ctx: CanvasRenderingContext2D, project: Project, style: Style, timeline: Timeline, view: { x: number; y: number; w: number }) {
    if (!style.clicks.ripple) return
    const grow = Math.sqrt(project.layout.w / view.w)
    for (const click of timeline.clicks) {
      const p = (this.t - click.at_ms) / RIPPLE_MS
      if (p < 0 || p >= 1) continue
      const [x, y] = this.placed(project, view, click.x, click.y)
      ctx.beginPath()
      ctx.arc(x, y, (12 + 30 * p) * grow, 0, Math.PI * 2)
      if (style.clicks.style === "filled") {
        ctx.fillStyle = style.clicks.color
        ctx.globalAlpha = ((1 - p) * 115) / 255
        ctx.fill()
      } else {
        ctx.strokeStyle = style.clicks.color
        ctx.globalAlpha = ((1 - p) * 230) / 255
        ctx.lineWidth = 3 * grow
        ctx.stroke()
      }
      ctx.globalAlpha = 1
    }
  }

  private drawCursor(ctx: CanvasRenderingContext2D, project: Project, style: Style, view: { x: number; y: number; w: number }) {
    const sample = sampleAt(project, this.t)
    if (!style.cursor.show || sample.opacity <= 0) return
    const { layout, sprite } = project
    const [x, y] = this.placed(project, view, sample.cx, sample.cy)
    const inside = x >= layout.x && x <= layout.x + layout.w && y >= layout.y && y <= layout.y + layout.h
    const image = this.images[POINTERS[sample.pointer]]
    if (!inside || !image.naturalWidth) return
    const k = sample.size / sprite.scale
    ctx.globalAlpha = sample.opacity
    ctx.drawImage(image, x - sprite.tip * k, y - sprite.tip * k, sprite.size * k, sprite.size * k)
    ctx.globalAlpha = 1
  }

  private drawGuides(ctx: CanvasRenderingContext2D, project: Project, timeline: Timeline, view: { x: number; y: number; w: number }) {
    const { selected } = getState()
    if (selected?.kind === "zoom") {
      const zoom = timeline.zooms[selected.i]
      if (!zoom) return
      const r = zoomRect(zoom, project.size)
      const [x, y] = this.placed(project, view, r.x, r.y)
      const k = project.layout.w / view.w
      const { layout } = project
      ctx.fillStyle = "rgba(0,0,0,.55)"
      ctx.beginPath()
      ctx.rect(layout.x, layout.y, layout.w, layout.h)
      ctx.rect(x, y, r.w * k, r.h * k)
      ctx.fill("evenodd")
      ctx.strokeStyle = GUIDE
      ctx.lineWidth = 3
      ctx.strokeRect(x, y, r.w * k, r.h * k)
    } else if (selected?.kind === "click") {
      const click = timeline.clicks[selected.i]
      if (!click) return
      const [x, y] = this.placed(project, view, click.x, click.y)
      ctx.strokeStyle = GUIDE
      ctx.lineWidth = 3
      ctx.beginPath()
      ctx.arc(x, y, 18, 0, Math.PI * 2)
      for (const [a, b] of [[-26, -10], [10, 26]]) {
        ctx.moveTo(x + a, y)
        ctx.lineTo(x + b, y)
        ctx.moveTo(x, y + a)
        ctx.lineTo(x, y + b)
      }
      ctx.stroke()
    }
  }

  seek(ms: number) {
    this.t = ms
    if (!this.video.paused) this.setPlaying(false)
    this.video.currentTime = ms / 1000
    this.draw()
    this.requestExact()
  }

  setPlaying(playing: boolean) {
    const { project } = getState()
    if (!project) return
    if (playing) {
      if (this.t >= project.duration_ms - 20) {
        this.t = 0
        this.video.currentTime = 0
      }
      this.hideExact()
      void this.video.play()
      setState({ playing: true })
      requestAnimationFrame(this.tick)
    } else {
      this.video.pause()
      setState({ playing: false })
      this.requestExact()
    }
  }

  toggle() {
    this.setPlaying(this.video.paused)
  }

  private tick = () => {
    if (this.video.paused) return
    this.t = this.video.currentTime * 1000
    const span = getState().timeline?.speeds.find((s) => this.t >= s.start_ms && this.t < s.end_ms)
    if (span?.speed === null) {
      this.video.currentTime = span.end_ms / 1000
      this.video.playbackRate = 1
    } else {
      this.video.playbackRate = span ? span.speed : 1
    }
    this.draw()
    requestAnimationFrame(this.tick)
  }

  private hideExact() {
    if (this.exact) this.exact.hidden = true
    this.exactWanted++
    clearTimeout(this.exactTimer)
  }

  private requestExact() {
    this.hideExact()
    const { project, selected } = getState()
    if (!this.video.paused || !project || selected?.kind === "zoom" || selected?.kind === "click") return
    const wanted = this.exactWanted
    this.exactTimer = window.setTimeout(async () => {
      const reply = await fetch(`/frame.png?ms=${this.t.toFixed(3)}&g=${project.generation}`)
      if (!reply.ok || wanted !== this.exactWanted || !this.exact) return
      const url = URL.createObjectURL(await reply.blob())
      if (wanted !== this.exactWanted) {
        URL.revokeObjectURL(url)
        return
      }
      const exact = this.exact
      exact.onload = () => {
        URL.revokeObjectURL(url)
        if (wanted === this.exactWanted) exact.hidden = false
      }
      exact.src = url
    }, 220)
  }
}

export const player = new Player()
