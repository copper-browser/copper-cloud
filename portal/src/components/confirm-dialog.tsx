"use client";

import { useId, useState } from "react";
import { toast } from "sonner";

import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/lib/api";
import { cn } from "@/lib/utils";

interface ConfirmDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: React.ReactNode;
  /** Consequences, rendered between description and footer. */
  children?: React.ReactNode;
  confirmLabel: string;
  pendingLabel?: string;
  destructive?: boolean;
  /** Require typing this exact text before the action enables. */
  confirmText?: string;
  onConfirm: () => Promise<void>;
  className?: string;
}

/** AlertDialog for destructive or significant actions. Esc / Cancel close it. */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  confirmLabel,
  pendingLabel,
  destructive = true,
  confirmText,
  onConfirm,
  className,
}: ConfirmDialogProps) {
  const [pending, setPending] = useState(false);
  const [typed, setTyped] = useState("");
  const inputId = useId();
  const armed = !confirmText || typed.trim().toLowerCase() === confirmText.toLowerCase();

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!armed || pending) return;
    setPending(true);
    try {
      await onConfirm();
      onOpenChange(false);
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setPending(false);
    }
  }

  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (pending) return;
        if (!next) setTyped("");
        onOpenChange(next);
      }}
    >
      <AlertDialogContent
        className={cn(
          "data-[size=default]:max-w-[calc(100%-2rem)] data-[size=default]:sm:max-w-[440px]",
          className,
        )}
      >
        <form onSubmit={submit} className="contents">
          <AlertDialogHeader>
            <AlertDialogTitle>{title}</AlertDialogTitle>
            {description && (
              <AlertDialogDescription>{description}</AlertDialogDescription>
            )}
          </AlertDialogHeader>
          {children}
          {confirmText && (
            <div className="grid gap-1.5">
              <Label
                htmlFor={inputId}
                className="block font-normal text-muted-foreground"
              >
                Type <span className="font-medium text-foreground">{confirmText}</span> to
                confirm
              </Label>
              <Input
                id={inputId}
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                autoComplete="off"
                spellCheck={false}
                autoFocus
              />
            </div>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={pending}>Cancel</AlertDialogCancel>
            <Button
              type="submit"
              variant={destructive ? "destructive-solid" : "default"}
              disabled={!armed || pending}
            >
              {pending ? (pendingLabel ?? `${confirmLabel}…`) : confirmLabel}
            </Button>
          </AlertDialogFooter>
        </form>
      </AlertDialogContent>
    </AlertDialog>
  );
}
