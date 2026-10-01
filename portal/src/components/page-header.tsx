import { cn } from "@/lib/utils";

interface PageHeaderProps {
  title: string;
  description?: React.ReactNode;
  /** The page's one primary action. */
  action?: React.ReactNode;
  className?: string;
}

export function PageHeader({ title, description, action, className }: PageHeaderProps) {
  return (
    <header className={cn("flex items-start justify-between gap-6 pt-7 pb-5", className)}>
      <div className="min-w-0">
        <h1 className="text-[19px]/7 font-semibold tracking-[-0.012em] text-foreground">
          {title}
        </h1>
        {description && (
          <div className="mt-0.5 max-w-[68ch] text-sm text-pretty text-muted-foreground">
            {description}
          </div>
        )}
      </div>
      {action && <div className="flex shrink-0 items-center gap-2 pt-0.5">{action}</div>}
    </header>
  );
}
