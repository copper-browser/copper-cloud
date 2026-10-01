"use client";

import { CheckIcon, CopyIcon } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useCopy } from "@/hooks/use-copy";
import { cn } from "@/lib/utils";

interface CopyButtonProps {
  value: string;
  /** What is being copied, for the accessible name and tooltip ("link code"). */
  label: string;
  className?: string;
  onCopied?: () => void;
  /** Show a text label next to the icon. */
  text?: string;
  variant?: "ghost" | "outline" | "secondary";
}

export function CopyButton({
  value,
  label,
  className,
  onCopied,
  text,
  variant = "ghost",
}: CopyButtonProps) {
  const { copy, copied } = useCopy();
  const onClick = async () => {
    if (await copy(value)) onCopied?.();
    else toast.error(`Couldn't copy the ${label}. Select it and copy manually.`);
  };
  const icon = copied ? (
    <CheckIcon className="text-success" aria-hidden="true" />
  ) : (
    <CopyIcon aria-hidden="true" />
  );

  if (text) {
    return (
      <Button
        type="button"
        variant={variant}
        size="sm"
        onClick={onClick}
        className={className}
        aria-label={`Copy ${label}`}
      >
        {icon}
        {copied ? "Copied" : text}
      </Button>
    );
  }

  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            type="button"
            variant={variant}
            size="icon-sm"
            onClick={onClick}
            className={cn("text-muted-foreground hover:text-foreground", className)}
            aria-label={`Copy ${label}`}
          />
        }
      >
        {icon}
      </TooltipTrigger>
      <TooltipContent>{copied ? "Copied" : `Copy ${label}`}</TooltipContent>
    </Tooltip>
  );
}
