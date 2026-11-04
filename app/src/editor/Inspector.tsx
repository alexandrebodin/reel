import { FolderOpenIcon, Trash2Icon } from "lucide-react"
import { useId } from "react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Separator } from "@/components/ui/separator"
import { Slider } from "@/components/ui/slider"
import { Switch } from "@/components/ui/switch"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"

import { markRemoved, timecode } from "./edits"
import { editStyle, editTimeline, endGesture, pickImage, useEditor } from "./store"
import type { Style } from "./types"

function SliderRow(props: { label: string; value: number; min: number; max: number; step: number; format: (v: number) => string; disabled?: boolean; onChange: (v: number) => void }) {
  const id = useId()
  return (
    <Field data-disabled={props.disabled || undefined} className="gap-2">
      <div className="flex items-center justify-between">
        <FieldLabel htmlFor={id}>{props.label}</FieldLabel>
        <span className="font-mono text-xs text-muted-foreground tabular-nums">{props.format(props.value)}</span>
      </div>
      <Slider id={id} aria-label={props.label} value={[props.value]} min={props.min} max={props.max} step={props.step} disabled={props.disabled} onValueChange={([v]) => props.onChange(v)} onValueCommit={endGesture} />
    </Field>
  )
}

function SwitchRow({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  const id = useId()
  return (
    <Field orientation="horizontal" className="justify-between">
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Switch id={id} className="ml-auto" checked={checked} onCheckedChange={onChange} />
    </Field>
  )
}

function ChoiceRow<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <Field className="gap-2">
      <FieldLabel>{label}</FieldLabel>
      <ToggleGroup type="single" variant="outline" size="sm" className="w-full" value={value} aria-label={label} onValueChange={(v) => v && onChange(v as T)}>
        {options.map(([option, name]) => (
          <ToggleGroupItem key={option} value={option} className="flex-1">
            {name}
          </ToggleGroupItem>
        ))}
      </ToggleGroup>
    </Field>
  )
}

function ColorSwatch({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
  return (
    <label className="relative block size-8 cursor-pointer overflow-hidden rounded-md ring-1 ring-border focus-within:ring-2 focus-within:ring-ring" style={{ background: value }}>
      <span className="sr-only">{label}</span>
      <input type="color" className="absolute inset-0 cursor-pointer opacity-0" value={value} onChange={(e) => onChange(e.target.value)} onBlur={endGesture} />
    </label>
  )
}

const times = (v: number) => `${v.toFixed(2)}×`
const percent = (v: number) => `${Math.round(v)}%`

function fillKind(fill: string) {
  if (fill === "none") return "none"
  if (fill.includes(",")) return "gradient"
  return fill.startsWith("#") ? "colour" : "image"
}

function BackgroundFill({ fill, canPick, set }: { fill: string; canPick: boolean; set: (fill: string, gesture?: boolean) => void }) {
  const kind = fillKind(fill)
  const choose = async () => {
    const path = await pickImage()
    if (path) set(path)
  }
  return (
    <>
      <Field className="gap-2">
        <FieldLabel>Fill</FieldLabel>
        <Select
          value={kind}
          onValueChange={(next) => {
            if (next === "none") set("none")
            if (next === "colour") set("#1f2937")
            if (next === "gradient") set("#2d1b69,#e2581f")
            if (next === "image") void choose()
          }}
        >
          <SelectTrigger className="w-full" aria-label="Fill">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              <SelectItem value="none">None</SelectItem>
              <SelectItem value="colour">Colour</SelectItem>
              <SelectItem value="gradient">Gradient</SelectItem>
              {(canPick || kind === "image") && <SelectItem value="image">Image…</SelectItem>}
            </SelectGroup>
          </SelectContent>
        </Select>
      </Field>
      {(kind === "colour" || kind === "gradient") && (
        <Field orientation="horizontal" className="justify-between">
          <FieldLabel>{kind === "colour" ? "Colour" : "Colours"}</FieldLabel>
          <div className="ml-auto flex gap-2">
            {fill.split(",").map((part, i, parts) => (
              <ColorSwatch key={i} label={parts.length > 1 ? (i ? "Gradient end" : "Gradient start") : "Colour"} value={part} onChange={(v) => set(parts.map((p, at) => (at === i ? v : p)).join(","), true)} />
            ))}
          </div>
        </Field>
      )}
      {kind === "image" &&
        (canPick ? (
          <Button variant="outline" size="sm" onClick={choose}>
            <FolderOpenIcon data-icon="inline-start" />
            <span className="truncate">{fill.split("/").pop()}</span>
          </Button>
        ) : (
          <Field className="gap-2">
            <FieldLabel htmlFor="image-path">Image path</FieldLabel>
            <Input id="image-path" defaultValue={fill} key={fill} onBlur={(e) => e.target.value && e.target.value !== fill && set(e.target.value)} />
          </Field>
        ))}
      {!canPick && kind !== "image" && <FieldDescription>To use an image, set background.fill to its path, or open this in the desktop app.</FieldDescription>}
    </>
  )
}

function Selected() {
  const selected = useEditor((s) => s.selected)
  const timeline = useEditor((s) => s.timeline)
  if (!selected || !timeline) return null
  const remove = () => editTimeline((t) => markRemoved(t, selected), null)
  const zoom = selected.kind === "zoom" ? timeline.zooms[selected.i] : null
  const speed = selected.kind === "speed" ? timeline.speeds[selected.i] : null
  const click = selected.kind === "click" ? timeline.clicks[selected.i] : null
  const title = zoom ? "Zoom" : speed ? (speed.speed === null ? "Cut" : "Speed") : "Click"
  const when = zoom ?? speed
  return (
    <div className="flex flex-col gap-4 p-4">
      <div className="flex items-center gap-2">
        <h2 className="text-sm font-medium">{title}</h2>
        <Badge variant="secondary" className="font-mono">
          {when ? `${timecode(when.start_ms)} – ${timecode(when.end_ms)}` : click && timecode(click.at_ms)}
        </Badge>
        <Button variant="ghost" size="icon-sm" className="ml-auto" aria-label={`Remove this ${title.toLowerCase()}`} onClick={remove}>
          <Trash2Icon />
        </Button>
      </div>
      <FieldGroup className="gap-4">
        {zoom && (
          <>
            <SliderRow label="Level" value={zoom.level} min={1} max={4} step={0.05} format={times} onChange={(level) => editTimeline((t) => ({ ...t, zooms: t.zooms.map((z, i) => (i === selected.i ? { ...z, level } : z)) }), selected, true)} />
            <FieldDescription>Drag on the picture to choose what the camera shows. Zooms that touch pan without zooming out.</FieldDescription>
          </>
        )}
        {speed && (
          <>
            <SwitchRow label="Cut it out" checked={speed.speed === null} onChange={(cut) => editTimeline((t) => ({ ...t, speeds: t.speeds.map((s, i) => (i === selected.i ? { ...s, speed: cut ? null : 2 } : s)) }))} />
            {speed.speed !== null && <SliderRow label="Speed" value={speed.speed} min={0.25} max={16} step={0.25} format={times} onChange={(value) => editTimeline((t) => ({ ...t, speeds: t.speeds.map((s, i) => (i === selected.i ? { ...s, speed: value } : s)) }), selected, true)} />}
          </>
        )}
        {click && <FieldDescription>Drag on the picture to place it.</FieldDescription>}
      </FieldGroup>
    </div>
  )
}

export function Inspector() {
  const project = useEditor((s) => s.project)
  const style = useEditor((s) => s.style)
  const edited = useEditor((s) => s.edited)
  const selected = useEditor((s) => s.selected)
  if (!project || !style) return <aside className="w-80 shrink-0 border-l" />

  const update = (change: (s: Style) => Style, gesture = false) => editStyle(change, gesture)
  const cursor = <K extends keyof Style["cursor"]>(key: K, value: Style["cursor"][K], gesture = false) => update((s) => ({ ...s, cursor: { ...s.cursor, [key]: value } }), gesture)
  const clicks = <K extends keyof Style["clicks"]>(key: K, value: Style["clicks"][K], gesture = false) => update((s) => ({ ...s, clicks: { ...s.clicks, [key]: value } }), gesture)
  const background = <K extends keyof Style["background"]>(key: K, value: Style["background"][K], gesture = false) => update((s) => ({ ...s, background: { ...s.background, [key]: value } }), gesture)
  const hasFill = style.background.fill !== "none"

  return (
    <aside className="w-80 shrink-0 border-l bg-card">
      <ScrollArea className="h-full">
        {selected && (
          <>
            <Selected />
            <Separator />
          </>
        )}
        <div className="flex flex-col gap-6 p-4">
          <FieldSet>
            <FieldLegend variant="label">Camera</FieldLegend>
            <FieldGroup className="gap-4">
              <SliderRow label="Automatic zoom" value={style.zoom.level} min={1} max={4} step={0.05} format={times} disabled={edited && !project.imported} onChange={(level) => update((s) => ({ ...s, zoom: { ...s.zoom, level } }), true)} />
              {edited && <FieldDescription>{project.imported ? "Takes effect when you reset to automatic." : "This timeline is edited. Select a zoom to change it, or reset to automatic."}</FieldDescription>}
              <ChoiceRow label="Zoom motion" value={style.zoom.animation} options={[["smooth", "Smooth"], ["dynamic", "Dynamic"]]} onChange={(animation) => update((s) => ({ ...s, zoom: { ...s.zoom, animation } }))} />
              <ChoiceRow label="Motion blur" value={style.motion_blur} options={[["off", "Off"], ["low", "Low"], ["medium", "Medium"], ["high", "High"]]} onChange={(motion_blur) => update((s) => ({ ...s, motion_blur }))} />
            </FieldGroup>
          </FieldSet>
          <Separator />
          <FieldSet>
            <FieldLegend variant="label">Cursor</FieldLegend>
            <FieldGroup className="gap-4">
              <SwitchRow label="Show cursor" checked={style.cursor.show} onChange={(v) => cursor("show", v)} />
              {project.imported && <FieldDescription>This video has its own cursor in the picture, which can't be removed. Turning this on draws a second one.</FieldDescription>}
              {style.cursor.show && (
                <>
                  <SliderRow label="Size" value={style.cursor.size} min={0.5} max={3} step={0.05} format={times} onChange={(v) => cursor("size", v, true)} />
                  <ChoiceRow label="Look" value={style.cursor.style} options={[["white_arrow", "White"], ["black_arrow", "Black"], ["dot", "Dot"]]} onChange={(v) => cursor("style", v)} />
                  <ChoiceRow label="Movement" value={style.cursor.motion} options={[["natural", "Natural"], ["smooth", "Smooth"]]} onChange={(v) => cursor("motion", v)} />
                  <SwitchRow label="Hand and text cursors" checked={style.cursor.adaptive} onChange={(v) => cursor("adaptive", v)} />
                  <SwitchRow label="Hide when idle" checked={style.cursor.hide_when_idle} onChange={(v) => cursor("hide_when_idle", v)} />
                  <SwitchRow label="Hide while typing" checked={style.cursor.hide_when_typing} onChange={(v) => cursor("hide_when_typing", v)} />
                </>
              )}
            </FieldGroup>
          </FieldSet>
          <Separator />
          <FieldSet>
            <FieldLegend variant="label">Clicks</FieldLegend>
            <FieldGroup className="gap-4">
              <SwitchRow label="Press effect" checked={style.clicks.press} onChange={(v) => clicks("press", v)} />
              <SwitchRow label="Ripple" checked={style.clicks.ripple} onChange={(v) => clicks("ripple", v)} />
              {style.clicks.ripple && (
                <>
                  <Field orientation="horizontal" className="justify-between">
                    <FieldLabel>Ripple colour</FieldLabel>
                    <div className="ml-auto">
                      <ColorSwatch label="Ripple colour" value={style.clicks.color} onChange={(v) => clicks("color", v, true)} />
                    </div>
                  </Field>
                  <ChoiceRow label="Ripple style" value={style.clicks.style} options={[["filled", "Filled"], ["outline", "Outline"]]} onChange={(v) => clicks("style", v)} />
                </>
              )}
            </FieldGroup>
          </FieldSet>
          <Separator />
          <FieldSet>
            <FieldLegend variant="label">Background</FieldLegend>
            <FieldGroup className="gap-4">
              <BackgroundFill fill={style.background.fill} canPick={project.can_pick} set={(fill, gesture) => background("fill", fill, gesture)} />
              <Field className="gap-2">
                <FieldLabel>Shape</FieldLabel>
                <Select value={style.background.aspect} onValueChange={(v) => background("aspect", v)}>
                  <SelectTrigger className="w-full" aria-label="Shape">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectItem value="auto">Same as video</SelectItem>
                      <SelectItem value="16:9">16:9</SelectItem>
                      <SelectItem value="4:3">4:3</SelectItem>
                      <SelectItem value="1:1">1:1</SelectItem>
                      <SelectItem value="9:16">9:16</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </Field>
              {hasFill && (
                <>
                  <SliderRow label="Padding" value={style.background.padding} min={0} max={30} step={0.5} format={percent} onChange={(v) => background("padding", v, true)} />
                  <SliderRow label="Shadow" value={style.background.shadow} min={0} max={100} step={1} format={percent} onChange={(v) => background("shadow", v, true)} />
                  <SliderRow label="Corners" value={style.background.corners} min={0} max={15} step={0.1} format={percent} onChange={(v) => background("corners", v, true)} />
                </>
              )}
            </FieldGroup>
          </FieldSet>
        </div>
      </ScrollArea>
    </aside>
  )
}
