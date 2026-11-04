export type Pointer = "arrow" | "hand" | "text"

export type ZoomSpan = { start_ms: number; end_ms: number; x: number; y: number; level: number }
/** `speed: null` cuts the stretch out. */
export type SpeedSpan = { start_ms: number; end_ms: number; speed: number | null }
export type Move = { start_ms: number; end_ms: number; x: number; y: number; pointer: Pointer }
export type Click = { at_ms: number; x: number; y: number }
export type Typing = { start_ms: number; end_ms: number }

export type Timeline = {
  zooms: ZoomSpan[]
  speeds: SpeedSpan[]
  moves: Move[]
  clicks: Click[]
  typing: Typing[]
}

export type Style = {
  zoom: { level: number; animation: "smooth" | "dynamic" }
  background: { fill: string; aspect: string; padding: number; shadow: number; corners: number }
  cursor: {
    show: boolean
    size: number
    style: "white_arrow" | "black_arrow" | "dot"
    motion: "natural" | "smooth"
    hide_when_idle: boolean
    hide_when_typing: boolean
    adaptive: boolean
  }
  clicks: { press: boolean; ripple: boolean; color: string; style: "filled" | "outline" }
  motion_blur: "off" | "low" | "medium" | "high"
}

export type Project = {
  generation: number
  name: string
  size: [number, number]
  scale: number
  fps: number
  duration_ms: number
  output_ms: number
  imported: boolean
  edited: boolean
  can_pick: boolean
  style: Style
  timeline: Timeline
  layout: { width: number; height: number; x: number; y: number; w: number; h: number; radius: number }
  sample_ms: number
  rows: number[]
  sprite: { size: number; tip: number; scale: number }
}

export type RenderStatus = {
  running: boolean
  progress: number
  finished: boolean
  error: string | null
  output: string
}

export type Selection = { kind: "zoom" | "speed" | "click"; i: number } | null
