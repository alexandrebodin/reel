import { PauseIcon, PlayIcon, SkipBackIcon } from "lucide-react"
import { useEffect, useRef } from "react"

import { Button } from "@/components/ui/button"

import { outputAt, timecode } from "./edits"
import { player } from "./player"
import { getState, useEditor } from "./store"

export function Transport() {
  const playing = useEditor((s) => s.playing)
  const project = useEditor((s) => s.project)
  const clock = useRef<HTMLSpanElement>(null)
  const output = useRef<HTMLSpanElement>(null)

  useEffect(
    () =>
      player.onFrame(() => {
        const { project, timeline } = getState()
        if (!project || !timeline) return
        if (clock.current) clock.current.textContent = `${timecode(player.t)} / ${timecode(project.duration_ms)}`
        if (output.current) output.current.textContent = `In the video: ${timecode(outputAt(timeline.speeds, player.t))} / ${timecode(project.output_ms)}`
      }),
    []
  )

  return (
    <div className="flex h-11 shrink-0 items-center justify-center gap-3 border-t px-3">
      <span ref={clock} className="min-w-0 flex-1 truncate text-right font-mono text-xs tabular-nums" />
      <Button variant="ghost" size="icon-sm" aria-label="Back to the start" disabled={!project} onClick={() => player.seek(0)}>
        <SkipBackIcon />
      </Button>
      <Button variant="secondary" size="icon" aria-label={playing ? "Pause" : "Play"} disabled={!project} onClick={() => player.toggle()}>
        {playing ? <PauseIcon /> : <PlayIcon />}
      </Button>
      <span ref={output} className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground tabular-nums" />
    </div>
  )
}
