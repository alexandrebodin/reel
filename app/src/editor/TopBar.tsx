import { DownloadIcon, FolderSearchIcon, Redo2Icon, RotateCcwIcon, Undo2Icon } from "lucide-react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Progress } from "@/components/ui/progress"
import { Separator } from "@/components/ui/separator"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"

import { closeRender, redo, resetToAutomatic, revealRender, startRender, undo, useEditor } from "./store"

export function TopBar() {
  const project = useEditor((s) => s.project)
  const edited = useEditor((s) => s.edited)
  const saving = useEditor((s) => s.saving)
  const canUndo = useEditor((s) => s.canUndo)
  const canRedo = useEditor((s) => s.canRedo)
  const render = useEditor((s) => s.render)
  const running = render?.running ?? false

  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b bg-card px-3">
      <h1 className="truncate text-sm font-medium">{project?.name ?? "reel"}</h1>
      <Badge variant="secondary">{edited ? "Edited" : "Automatic"}</Badge>
      <span className="truncate text-xs text-muted-foreground">{saving ? "Saving…" : project && `Plays in ${(project.output_ms / 1000).toFixed(1)} s`}</span>
      <div className="ml-auto flex items-center gap-1">
        <IconAction label="Undo (⌘Z)" disabled={!canUndo} onClick={undo}>
          <Undo2Icon />
        </IconAction>
        <IconAction label="Redo (⇧⌘Z)" disabled={!canRedo} onClick={redo}>
          <Redo2Icon />
        </IconAction>
        <IconAction label="Reset to automatic" disabled={!edited} onClick={resetToAutomatic}>
          <RotateCcwIcon />
        </IconAction>
        <Separator orientation="vertical" className="mx-1 h-5" />
        <Button size="sm" disabled={running || !project} onClick={startRender}>
          {running ? <Spinner data-icon="inline-start" /> : <DownloadIcon data-icon="inline-start" />}
          {running ? `Rendering ${Math.round((render?.progress ?? 0) * 100)}%` : "Render"}
        </Button>
      </div>
      {running && <Progress value={(render?.progress ?? 0) * 100} className="fixed inset-x-0 top-12 h-0.5 rounded-none" />}
      <Dialog open={render?.finished ?? false} onOpenChange={(open) => !open && closeRender()}>
        <DialogContent className="sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>Render finished</DialogTitle>
            <DialogDescription className="truncate font-mono text-xs">{render?.output}</DialogDescription>
          </DialogHeader>
          {render?.finished && <video src="/output.mp4" controls autoPlay muted className="w-full rounded-md ring-1 ring-border" />}
          <DialogFooter>
            <Button variant="outline" onClick={revealRender}>
              <FolderSearchIcon data-icon="inline-start" />
              Show in folder
            </Button>
            <Button onClick={closeRender}>Done</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </header>
  )
}

function IconAction({ label, disabled, onClick, children }: { label: string; disabled: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span>
          <Button variant="ghost" size="icon-sm" aria-label={label} disabled={disabled} onClick={onClick}>
            {children}
          </Button>
        </span>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}
