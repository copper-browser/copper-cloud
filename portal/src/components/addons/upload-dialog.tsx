"use client";

import { CircleAlertIcon, FileArchiveIcon, UploadIcon } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import { formatBytes } from "@/components/addons/util";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { api, ApiError, errorMessage } from "@/lib/api";
import type { Addon, AddonSlot, AddonUpload } from "@/lib/types";
import { cn } from "@/lib/utils";

const ACCEPT = ".copperaddon,.zip,application/zip";
const looksLikeBundle = (f: File) => /\.(copperaddon|zip)$/i.test(f.name);

function uploadError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 413 || error.code === "payload_too_large") {
      return "That file is larger than this server accepts.";
    }
    return error.message;
  }
  return errorMessage(error);
}

interface UploadDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  slots: AddonSlot[];
  /** Preselected slot (from `?slot=` or the group the button sits in). */
  slot?: string;
  /** Set when uploading a new version of this addon. */
  target?: Addon | null;
  onUploaded: (result: AddonUpload) => void;
}

/** Pick a slot and a `.copperaddon` file; the server validates and answers inline. */
export function UploadDialog({
  open,
  onOpenChange,
  slots,
  slot: initialSlot,
  target,
  onUploaded,
}: UploadDialogProps) {
  const [slot, setSlot] = useState<string>("");
  const [file, setFile] = useState<File | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [slotError, setSlotError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [dragging, setDragging] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const ids = { slot: useId(), file: useId(), fileHint: useId() };

  // Fresh form each time it opens.
  useEffect(() => {
    if (!open) return;
    setSlot(target?.slot ?? initialSlot ?? (slots.length === 1 ? slots[0]!.id : ""));
    setFile(null);
    setError(null);
    setFileError(null);
    setSlotError(null);
    setDragging(false);
  }, [open, target, initialSlot, slots]);

  const items = slots.map((s) => ({ value: s.id, label: s.name }));
  const chosenSlot = slots.find((s) => s.id === slot);

  function pick(next: File | null | undefined) {
    if (!next) return;
    setError(null);
    if (!looksLikeBundle(next)) {
      setFileError("Choose a .copperaddon or .zip file.");
      return;
    }
    setFileError(null);
    setFile(next);
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const missingSlot = !slot;
    if (missingSlot) setSlotError("Choose the slot this addon is built for.");
    if (!file) setFileError("Choose the addon file to upload.");
    if (missingSlot || !file) return;
    setPending(true);
    setError(null);
    try {
      onUploaded(await api.addons.upload(slot, file));
    } catch (err) {
      setError(uploadError(err));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (pending) return;
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-[460px]">
        <form onSubmit={submit} className="contents" noValidate>
          <DialogHeader>
            <DialogTitle>
              {target ? `New version of ${target.name}` : "Upload addon"}
            </DialogTitle>
            <DialogDescription>
              {target ? (
                <>
                  The file&rsquo;s manifest must keep the id{" "}
                  <code className="font-mono text-xs text-foreground">{target.id}</code>{" "}
                  and a new version number. It becomes current right away.
                </>
              ) : (
                "New addons start off, with nobody selected, so you can check the settings first."
              )}
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor={ids.slot}>Slot</Label>
              <Select
                items={items}
                value={slot || null}
                onValueChange={(v) => {
                  setSlot(v ? String(v) : "");
                  setSlotError(null);
                  setError(null);
                }}
                disabled={!!target || pending}
              >
                <SelectTrigger
                  id={ids.slot}
                  className="w-full"
                  aria-invalid={slotError ? true : undefined}
                >
                  <SelectValue placeholder="Choose a slot" />
                </SelectTrigger>
                <SelectContent>
                  {items.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p
                className={cn(
                  "text-xs text-pretty",
                  slotError ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {slotError ??
                  (chosenSlot
                    ? chosenSlot.description
                    : "The part of Copper the addon replaces. It must match the manifest.")}
              </p>
            </div>

            <div className="grid gap-1.5">
              <Label htmlFor={ids.file}>File</Label>
              <input
                ref={inputRef}
                id={ids.file}
                type="file"
                accept={ACCEPT}
                className="peer sr-only"
                disabled={pending}
                aria-describedby={ids.fileHint}
                aria-invalid={fileError ? true : undefined}
                onChange={(e) => {
                  pick(e.target.files?.[0]);
                  e.target.value = "";
                }}
              />
              <label
                htmlFor={ids.file}
                onDragOver={(e) => {
                  e.preventDefault();
                  setDragging(true);
                }}
                onDragLeave={() => setDragging(false)}
                onDrop={(e) => {
                  e.preventDefault();
                  setDragging(false);
                  pick(e.dataTransfer.files?.[0]);
                }}
                className={cn(
                  "flex h-[76px] cursor-pointer items-center gap-3 rounded-lg border border-dashed border-input px-3.5 transition-colors peer-focus-visible:border-ring peer-focus-visible:ring-3 peer-focus-visible:ring-ring/50 hover:bg-muted/50",
                  dragging && "border-ring bg-muted/60",
                  fileError && "border-destructive",
                )}
              >
                <span className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-well">
                  {file ? (
                    <FileArchiveIcon
                      className="size-4 text-muted-foreground"
                      aria-hidden="true"
                    />
                  ) : (
                    <UploadIcon
                      className="size-4 text-muted-foreground"
                      aria-hidden="true"
                    />
                  )}
                </span>
                {file ? (
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium" title={file.name}>
                      {file.name}
                    </span>
                    <span className="block text-xs text-muted-foreground">
                      {formatBytes(file.size)} · Click to choose another
                    </span>
                  </span>
                ) : (
                  <span className="min-w-0 flex-1">
                    <span className="block font-medium">Choose a .copperaddon file</span>
                    <span className="block text-xs text-muted-foreground">
                      Or drop it here.
                    </span>
                  </span>
                )}
              </label>
              <p
                id={ids.fileHint}
                className={cn(
                  "text-xs text-pretty",
                  fileError ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {fileError ?? "A zip with manifest.json at its root."}
              </p>
            </div>

            {error && (
              <div
                role="alert"
                className="flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5 text-sm"
              >
                <CircleAlertIcon
                  className="mt-0.5 size-4 shrink-0 text-destructive"
                  aria-hidden="true"
                />
                <div className="min-w-0">
                  <p className="font-medium">The server didn&rsquo;t accept this file</p>
                  <p className="mt-0.5 text-pretty wrap-break-word text-muted-foreground">
                    {error}
                  </p>
                </div>
              </div>
            )}
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? "Uploading…" : "Upload"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
