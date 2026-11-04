import { useSyncExternalStore } from "react"
import { toast } from "sonner"

import type { Project, RenderStatus, Selection, Style, Timeline } from "./types"

export type EditorState = {
  project: Project | null
  style: Style | null
  timeline: Timeline | null
  /** False while the timeline still follows the automatic pass. */
  edited: boolean
  selected: Selection
  canUndo: boolean
  canRedo: boolean
  saving: boolean
  playing: boolean
  render: RenderStatus | null
  loadError: string | null
}

type Document = Pick<EditorState, "style" | "timeline" | "edited">

let state: EditorState = {
  project: null,
  style: null,
  timeline: null,
  edited: false,
  selected: null,
  canUndo: false,
  canRedo: false,
  saving: false,
  playing: false,
  render: null,
  loadError: null,
}
const listeners = new Set<() => void>()
let undoStack: Document[] = []
let redoStack: Document[] = []
let lastGood: Document | null = null
let gestureOpen = false
let sending = false
let sendAgain = false

export function getState() {
  return state
}

export function setState(change: Partial<EditorState>) {
  state = { ...state, ...change, canUndo: undoStack.length > 0, canRedo: redoStack.length > 0 }
  listeners.forEach((listener) => listener())
}

export function useEditor<T>(selector: (state: EditorState) => T): T {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },
    () => selector(state)
  )
}

const documentOf = ({ style, timeline, edited }: EditorState): Document => ({ style, timeline, edited })

export async function api<T>(path: string, body?: unknown): Promise<T> {
  const reply = await fetch(path, body === undefined ? {} : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) })
  const data = await reply.json()
  if (!reply.ok) throw new Error(data.error)
  return data
}

/** The server's answer replaces the timeline only while it is automatic: once edited, this page is the source and the answer may already be out of date. */
function adopt(project: Project) {
  if (!state.edited) {
    setState({ project, timeline: project.timeline })
    lastGood = documentOf(state)
  } else {
    setState({ project })
    if (!sendAgain) lastGood = documentOf(state)
  }
}

async function send() {
  if (sending) {
    sendAgain = true
    return
  }
  sending = true
  setState({ saving: true })
  try {
    adopt(await api<Project>("/api/edit", { style: state.style, timeline: state.edited ? state.timeline : null }))
  } catch (error) {
    toast.error((error as Error).message)
    gestureOpen = false
    sendAgain = false
    if (lastGood) setState(lastGood)
  }
  sending = false
  if (sendAgain) {
    sendAgain = false
    send()
  } else {
    setState({ saving: false })
  }
}

function remember() {
  undoStack.push(documentOf(state))
  redoStack = []
}

/** Changes the look. Inside a gesture, one undo covers all of it. */
export function editStyle(change: (style: Style) => Style, gesture = false) {
  if (!gesture || !gestureOpen) remember()
  gestureOpen = gesture
  setState({ style: change(state.style!) })
  send()
}

export function endGesture() {
  gestureOpen = false
}

/** Changes the timeline and saves it. */
export function editTimeline(change: (timeline: Timeline) => Timeline, selected: Selection = state.selected, gesture = false) {
  if (!gesture || !gestureOpen) remember()
  gestureOpen = gesture
  setState({ timeline: change(state.timeline!), edited: true, selected })
  send()
}

/** Starts a drag: an undo point now, nothing saved until `finishDrag`. */
export function beginDrag() {
  remember()
  setState({ edited: true })
}

export function dragTimeline(change: (timeline: Timeline) => Timeline, selected: Selection = state.selected) {
  setState({ timeline: change(state.timeline!), selected })
}

export function finishDrag() {
  send()
}

export function select(selected: Selection) {
  setState({ selected })
}

export function undo() {
  const previous = undoStack.pop()
  if (!previous) return
  redoStack.push(documentOf(state))
  setState({ ...previous, selected: null })
  send()
}

export function redo() {
  const next = redoStack.pop()
  if (!next) return
  undoStack.push(documentOf(state))
  setState({ ...next, selected: null })
  send()
}

export async function resetToAutomatic() {
  remember()
  setState({ saving: true, selected: null })
  try {
    const project = await api<Project>("/api/auto", {})
    setState({ project, style: project.style, timeline: project.timeline, edited: project.edited, saving: false })
    lastGood = documentOf(state)
  } catch (error) {
    undoStack.pop()
    setState({ saving: false })
    toast.error((error as Error).message)
  }
}

export async function load() {
  try {
    const project = await api<Project>("/api/project")
    document.title = `${project.name} · reel`
    setState({ project, style: project.style, timeline: project.timeline, edited: project.edited })
    lastGood = documentOf(state)
    const render = await api<RenderStatus>("/api/render")
    if (render.running) watchRender()
  } catch (error) {
    setState({ loadError: (error as Error).message })
  }
}

async function watchRender() {
  const render = await api<RenderStatus>("/api/render")
  setState({ render })
  if (render.running) setTimeout(watchRender, 400)
  else if (render.error) toast.error(`The render failed: ${render.error}`)
}

export async function startRender() {
  try {
    await api("/api/render", {})
    setState({ render: { running: true, progress: 0, finished: false, error: null, output: "" } })
    watchRender()
  } catch (error) {
    toast.error((error as Error).message)
  }
}

export function closeRender() {
  setState({ render: null })
}

export async function revealRender() {
  try {
    await api("/api/reveal", {})
  } catch (error) {
    toast.error((error as Error).message)
  }
}

export async function pickImage(): Promise<string | null> {
  try {
    return (await api<{ path: string | null }>("/api/pick-image", {})).path
  } catch (error) {
    toast.error((error as Error).message)
    return null
  }
}
