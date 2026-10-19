"use client";

import { TriangleAlertIcon, UploadIcon } from "lucide-react";
import { Fragment, useId, useState } from "react";
import { toast } from "sonner";

import { PeoplePicker } from "@/components/addons/people-picker";
import { SettingsForm } from "@/components/addons/settings-form";
import {
  formatBytes,
  reachesNobody,
  settingSpecs,
  shortSha,
} from "@/components/addons/util";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { RelativeTime } from "@/components/relative-time";
import { RowActions } from "@/components/row-actions";
import { Segmented } from "@/components/segmented";
import { StatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { api, errorMessage } from "@/lib/api";
import { plural } from "@/lib/format";
import type { Addon, AddonAudience, AddonPatch, AddonPerson } from "@/lib/types";
import { cn } from "@/lib/utils";

const AUDIENCE: { value: AddonAudience; label: string }[] = [
  { value: "everyone", label: "Everyone" },
  { value: "people", label: "Only these people" },
];

const PLATFORM_NAMES = { mac: "Mac", linux: "Linux", windows: "Windows" } as const;

function audienceLine(a: Addon): string {
  const n = a.people.length;
  if (a.audience === "everyone") {
    return a.enabled
      ? "Everyone signed in to this instance gets it."
      : "Everyone signed in to this instance, once it’s on.";
  }
  if (n === 0) return "Nobody yet. Add people by name or email.";
  const who = n === 1 ? "This person" : `These ${n} people`;
  return a.enabled ? `${who} get${n === 1 ? "s" : ""} it.` : `${who}, once it’s on.`;
}

function turnedOnMessage(a: Addon): string {
  if (!a.enabled) return `${a.name} is off`;
  if (a.audience === "everyone") return `${a.name} is on for everyone`;
  if (a.people.length === 0) return `${a.name} is on, but nobody is selected`;
  return `${a.name} is on for ${plural(a.people.length, "person", "people")}`;
}

interface AddonCardProps {
  addon: Addon;
  /** Other addons in the same slot; priority only matters when there are some. */
  siblings: number;
  onChange: (addon: Addon) => void;
  onRemoved: (id: string) => void;
  onUploadVersion: (addon: Addon) => void;
}

export function AddonCard({
  addon,
  siblings,
  onChange,
  onRemoved,
  onUploadVersion,
}: AddonCardProps) {
  const [switching, setSwitching] = useState(false);
  const [makingCurrent, setMakingCurrent] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const ids = {
    title: useId(),
    reach: useId(),
    audience: useId(),
    settings: useId(),
    versions: useId(),
    priority: useId(),
  };
  const manifest = addon.manifest;
  const specs = settingSpecs(addon);
  const platforms = manifest?.platforms;
  const limited = platforms && platforms.length > 0 && platforms.length < 3;

  async function patch(input: AddonPatch, success?: (next: Addon) => string) {
    try {
      const next = await api.addons.update(addon.id, input);
      onChange(next);
      if (success) toast.success(success(next));
      return true;
    } catch (error) {
      toast.error(`Couldn't save. ${errorMessage(error)}`);
      return false;
    }
  }

  /** Show the change at once; roll back if the server refuses it. */
  async function optimistic(local: Partial<Addon>, input: AddonPatch) {
    const before = addon;
    onChange({ ...addon, ...local });
    const ok = await patch(input);
    if (!ok) onChange(before);
  }

  async function toggle(enabled: boolean) {
    setSwitching(true);
    await patch({ enabled }, turnedOnMessage);
    setSwitching(false);
  }

  function setPeople(people: AddonPerson[]) {
    void optimistic({ people }, { people: people.map((p) => p.email) });
  }

  async function makeCurrent(version: string) {
    setMakingCurrent(version);
    await patch(
      { current_version: version },
      (next) => `${next.name} ${version} is now current`,
    );
    setMakingCurrent(null);
  }

  return (
    <article
      aria-labelledby={ids.title}
      className="@container/card rounded-lg border bg-card"
    >
      <header className="p-4">
        <div className="flex items-start gap-4">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
              <h3 id={ids.title} className="text-sm font-medium text-foreground">
                {addon.name}
              </h3>
              {addon.current && (
                <span className="font-mono text-xs text-muted-foreground">
                  {addon.current.version}
                </span>
              )}
            </div>
            <p className="mt-0.5 flex min-w-0 flex-wrap gap-x-1.5 text-xs text-muted-foreground">
              <code className="font-mono">
                {addon.id.split(/(?<=[.-])/).map((part, i) => (
                  <Fragment key={i}>
                    {i > 0 && <wbr />}
                    {part}
                  </Fragment>
                ))}
              </code>
              {manifest?.author && (
                <span className="truncate">
                  <span aria-hidden="true">· </span>by {manifest.author}
                </span>
              )}
              {limited && (
                <span>
                  <span aria-hidden="true">· </span>
                  {platforms.map((p) => PLATFORM_NAMES[p] ?? p).join(" and ")} only
                </span>
              )}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <span
              aria-hidden="true"
              className={cn(
                "w-6 text-right text-xs font-medium",
                addon.enabled ? "text-foreground" : "text-muted-foreground",
              )}
            >
              {addon.enabled ? "On" : "Off"}
            </span>
            <Switch
              checked={addon.enabled}
              disabled={switching}
              onCheckedChange={(checked) => void toggle(checked)}
              aria-label={`${addon.name} on`}
              aria-describedby={ids.reach}
            />
            <RowActions label={`More actions for ${addon.name}`}>
              <DropdownMenuItem onClick={() => onUploadVersion(addon)}>
                <UploadIcon aria-hidden="true" />
                Upload new version…
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onClick={() => setDeleting(true)}>
                Delete addon…
              </DropdownMenuItem>
            </RowActions>
          </div>
        </div>
        {manifest?.description && (
          <p className="mt-2 max-w-[60ch] text-pretty text-muted-foreground">
            {manifest.description}
          </p>
        )}
      </header>

      {addon.enabled && reachesNobody(addon) && (
        <div
          role="status"
          className="mx-4 mb-4 flex items-start gap-2.5 rounded-md border border-warning/40 bg-warning-surface px-3 py-2.5 text-warning-foreground"
        >
          <TriangleAlertIcon
            className="mt-0.5 size-4 shrink-0 text-warning"
            aria-hidden="true"
          />
          <p className="text-sm text-pretty">
            <span className="font-semibold">Nobody will get it.</span> It&rsquo;s on, but
            no one is selected. Add people below or choose Everyone.
          </p>
        </div>
      )}

      <section aria-labelledby={ids.audience} className="grid gap-3 border-t p-4">
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
          <div className="min-w-0">
            <h4 id={ids.audience} className="text-sm font-medium">
              Who gets it
            </h4>
            <p id={ids.reach} className="mt-0.5 text-pretty text-muted-foreground">
              {audienceLine(addon)}
            </p>
          </div>
          <Segmented
            label="Who gets it"
            value={addon.audience}
            onChange={(audience) => {
              if (audience !== addon.audience)
                void optimistic({ audience }, { audience });
            }}
            options={AUDIENCE}
          />
        </div>
        {addon.audience === "people" ? (
          <PeoplePicker
            label={`Add people to ${addon.name}`}
            value={addon.people}
            onChange={setPeople}
          />
        ) : (
          addon.people.length > 0 && (
            <p className="text-xs text-muted-foreground">
              The {plural(addon.people.length, "person", "people")} you picked stay saved
              for Only these people.
            </p>
          )
        )}
        {siblings > 0 && <PriorityField addon={addon} id={ids.priority} patch={patch} />}
      </section>

      <section aria-labelledby={ids.settings} className="border-t p-4">
        <SettingsForm
          key={`${addon.current?.sha256 ?? ""}:${JSON.stringify(addon.settings)}`}
          headingId={ids.settings}
          specs={specs}
          values={addon.settings}
          onSave={async (values) => {
            try {
              const next = await api.addons.update(addon.id, { settings: values });
              onChange(next);
              toast.success(`Saved settings for ${next.name}`);
              return null;
            } catch (error) {
              return errorMessage(error);
            }
          }}
        />
      </section>

      <section aria-labelledby={ids.versions} className="border-t p-4">
        <div className="flex min-h-7 items-center justify-between gap-3">
          <h4 id={ids.versions} className="text-sm font-medium">
            Versions
          </h4>
          <Button
            variant="outline"
            size="sm"
            className="-mr-1"
            onClick={() => onUploadVersion(addon)}
          >
            <UploadIcon aria-hidden="true" />
            Upload new version
          </Button>
        </div>
        {addon.versions.length === 0 ? (
          <p className="mt-2 text-muted-foreground">No versions uploaded.</p>
        ) : (
          <ul aria-label={`${addon.name} versions`} className="mt-2">
            {addon.versions.map((v) => {
              const current = v.version === addon.current?.version;
              return (
                <li
                  key={v.version}
                  className="flex h-10 items-center gap-3 border-b border-border/70 last:border-b-0"
                >
                  <span
                    className="w-14 shrink-0 truncate font-mono text-xs text-foreground @sm/card:w-[72px]"
                    title={v.version}
                  >
                    {v.version}
                  </span>
                  <span
                    className="min-w-0 flex-1 truncate text-muted-foreground"
                    title={[
                      v.uploaded_by && `Uploaded by ${v.uploaded_by}`,
                      `SHA-256 ${v.sha256}`,
                    ]
                      .filter(Boolean)
                      .join("\n")}
                  >
                    <RelativeTime value={v.created_at} />
                    {v.uploaded_by && (
                      <span className="hidden @xl/card:inline"> by {v.uploaded_by}</span>
                    )}
                  </span>
                  <code
                    className="hidden shrink-0 font-mono text-xs text-faint @2xl/card:block"
                    title={`SHA-256 ${v.sha256}`}
                  >
                    {shortSha(v.sha256)}
                  </code>
                  <span className="w-16 shrink-0 text-right text-muted-foreground tabular-nums @max-sm/card:hidden">
                    {formatBytes(v.size_bytes)}
                  </span>
                  <span className="flex w-[104px] shrink-0 justify-end">
                    {current ? (
                      <StatusBadge tone="success">Current</StatusBadge>
                    ) : (
                      <Button
                        variant="ghost"
                        size="sm"
                        className="-mr-1.5"
                        disabled={makingCurrent !== null}
                        onClick={() => void makeCurrent(v.version)}
                        aria-label={`Make ${v.version} current`}
                      >
                        {makingCurrent === v.version ? "Switching…" : "Make current"}
                      </Button>
                    )}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={`Delete “${addon.name}”?`}
        description={`This removes it and its ${plural(addon.versions.length, "version")}. Coppers go back to the built-in pane on their next check. This can't be undone.`}
        confirmLabel="Delete addon"
        pendingLabel="Deleting…"
        onConfirm={async () => {
          await api.addons.remove(addon.id);
          onRemoved(addon.id);
          toast.success(`Deleted “${addon.name}”`);
        }}
      />
    </article>
  );
}

function PriorityField({
  addon,
  id,
  patch,
}: {
  addon: Addon;
  id: string;
  patch: (input: AddonPatch, success?: (next: Addon) => string) => Promise<boolean>;
}) {
  const [value, setValue] = useState(String(addon.priority));
  const [error, setError] = useState<string>();

  async function commit() {
    const trimmed = value.trim();
    if (trimmed === String(addon.priority)) return;
    if (!/^-?\d+$/.test(trimmed)) {
      setError("Use a whole number.");
      return;
    }
    setError(undefined);
    const ok = await patch(
      { priority: Number(trimmed) },
      (next) => `${next.name} priority set to ${next.priority}`,
    );
    if (!ok) setValue(String(addon.priority));
  }

  return (
    <div className="grid gap-1.5 border-t pt-3">
      <div className="flex items-center gap-3">
        <Label htmlFor={id}>Priority</Label>
        <Input
          id={id}
          type="number"
          inputMode="numeric"
          step={1}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onBlur={() => void commit()}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void commit();
            }
          }}
          className="w-20"
          aria-invalid={error ? true : undefined}
          aria-describedby={`${id}-hint`}
        />
      </div>
      <p
        id={`${id}-hint`}
        className={cn(
          "text-xs text-pretty",
          error ? "text-destructive" : "text-muted-foreground",
        )}
      >
        {error ??
          "When more than one addon in this slot is on for a person, the highest priority wins."}
      </p>
    </div>
  );
}
