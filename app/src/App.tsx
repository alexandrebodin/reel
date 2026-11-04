import { useEffect } from "react"

import { Toaster } from "@/components/ui/sonner"
import { TooltipProvider } from "@/components/ui/tooltip"
import { clamp, markRemoved } from "@/editor/edits"
import { Inspector } from "@/editor/Inspector"
import { player } from "@/editor/player"
import { Preview } from "@/editor/Preview"
import { editTimeline, getState, load, redo, select, undo, useEditor } from "@/editor/store"
import { addMark, TimelinePanel } from "@/editor/Timeline"
import { TopBar } from "@/editor/TopBar"
import { Transport } from "@/editor/Transport"

function removeSelected() {
  const { selected } = getState()
  if (selected) editTimeline((t) => markRemoved(t, selected), null)
}

function onKeyDown(event: KeyboardEvent) {
  const target = event.target as HTMLElement
  const { project } = getState()
  if (!project || target.matches("input, textarea, select, [role=combobox], [role=listbox], [role=option], [role=dialog] *")) return
  const command = event.metaKey || event.ctrlKey
  if (command && event.key.toLowerCase() === "z") {
    event.preventDefault()
    if (event.shiftKey) redo()
    else undo()
    return
  }
  if (command) return
  if (event.key === " ") {
    if (target.tagName === "BUTTON") return
    event.preventDefault()
    player.toggle()
  } else if ((event.key === "ArrowLeft" || event.key === "ArrowRight") && target.getAttribute("role") !== "slider") {
    event.preventDefault()
    const step = (event.shiftKey ? 1000 : 1000 / project.fps) * (event.key === "ArrowLeft" ? -1 : 1)
    player.seek(clamp(player.t + step, 0, project.duration_ms))
  } else if (event.key === "Backspace" || event.key === "Delete") removeSelected()
  else if (event.key === "Escape") select(null)
  else if (event.key === "z") addMark("zoom", player.t)
  else if (event.key === "s") addMark("speed", player.t)
  else if (event.key === "c") addMark("click", player.t)
}

export function App() {
  const loadError = useEditor((s) => s.loadError)

  useEffect(() => {
    void load()
    addEventListener("keydown", onKeyDown)
    return () => removeEventListener("keydown", onKeyDown)
  }, [])

  if (loadError) return <p className="p-6 text-sm text-destructive">{loadError}</p>

  return (
    <TooltipProvider>
      <div className="flex h-svh flex-col overflow-hidden">
        <TopBar />
        <div className="flex min-h-0 flex-1">
          <main className="flex min-w-0 flex-1 flex-col">
            <div className="flex min-h-0 flex-1 p-4">
              <Preview />
            </div>
            <Transport />
          </main>
          <Inspector />
        </div>
        <TimelinePanel />
      </div>
      <Toaster position="bottom-right" />
    </TooltipProvider>
  )
}

export default App
