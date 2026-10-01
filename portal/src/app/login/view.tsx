"use client";

import { CircleAlertIcon } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import { BrandMark } from "@/components/brand-mark";
import { MockBanner } from "@/components/mock-banner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api, ApiError, errorMessage, MOCK_MODE } from "@/lib/api";

/** Only same-origin paths; never protocol-relative or absolute URLs. */
function safeNext(): string {
  const next = new URLSearchParams(window.location.search).get("next");
  if (
    next &&
    next.startsWith("/") &&
    !next.startsWith("//") &&
    !next.startsWith("/login")
  ) {
    return next;
  }
  return "/";
}

export function LoginView() {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [host, setHost] = useState<string | null>(null);
  const passwordRef = useRef<HTMLInputElement>(null);
  const emailId = useId();
  const passwordId = useId();
  const errorId = useId();

  useEffect(() => {
    setHost(window.location.host);
    // Already signed in? Skip the form.
    const controller = new AbortController();
    api
      .me({ signal: controller.signal, redirectOn401: false })
      .then(() => window.location.replace(safeNext()))
      .catch(() => {});
    return () => controller.abort();
  }, []);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    setError(null);
    setPending(true);
    try {
      await api.login(email.trim(), password);
      window.location.replace(safeNext());
    } catch (err) {
      setPending(false);
      if (err instanceof ApiError && err.code === "credentials") {
        setError("Wrong email or password.");
        setPassword("");
        passwordRef.current?.focus();
      } else if (err instanceof ApiError && err.code === "rate_limited") {
        setError("Too many attempts. Wait a minute, then try again.");
      } else {
        setError(errorMessage(err));
      }
    }
  }

  return (
    <div className="flex min-h-dvh flex-col bg-rail">
      <MockBanner />
      <main className="flex flex-1 flex-col items-center justify-center px-4 py-12">
        <div className="w-full max-w-[368px]">
          <div className="mb-6 flex items-center gap-2.5">
            <BrandMark className="size-6" />
            <span className="text-[15px] font-semibold tracking-tight">Copper Cloud</span>
          </div>

          <div className="rounded-xl border bg-background p-6 shadow-[0_1px_2px_rgb(0_0_0/0.04),0_8px_24px_-12px_rgb(0_0_0/0.12)]">
            <h1 className="text-base font-semibold tracking-tight">
              Sign in to the admin console
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              {host ? (
                <>
                  Manage access, people and devices on{" "}
                  <span className="font-medium text-foreground">{host}</span>.
                </>
              ) : (
                "Manage access, people and devices on this instance."
              )}
            </p>

            <form onSubmit={submit} className="mt-5 grid gap-4" noValidate>
              <div className="grid gap-1.5">
                <Label htmlFor={emailId}>Email</Label>
                <Input
                  id={emailId}
                  type="email"
                  name="email"
                  autoComplete="username"
                  autoFocus
                  required
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  aria-invalid={error ? true : undefined}
                  aria-describedby={error ? errorId : undefined}
                  placeholder="admin@example.com"
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor={passwordId}>Password</Label>
                <Input
                  ref={passwordRef}
                  id={passwordId}
                  type="password"
                  name="password"
                  autoComplete="current-password"
                  required
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  aria-invalid={error ? true : undefined}
                  aria-describedby={error ? errorId : undefined}
                />
              </div>

              {error && (
                <p
                  id={errorId}
                  role="alert"
                  className="-mt-1 flex items-start gap-1.5 text-sm text-destructive"
                >
                  <CircleAlertIcon
                    className="mt-0.5 size-3.5 shrink-0"
                    aria-hidden="true"
                  />
                  {error}
                </p>
              )}

              <Button
                type="submit"
                size="lg"
                className="w-full"
                disabled={pending || !email.trim() || !password}
              >
                {pending ? "Signing in…" : "Sign in"}
              </Button>
            </form>
          </div>

          <div className="mt-5 px-1 text-xs/5 text-muted-foreground">
            <p>Locked out? On the server, run:</p>
            <code className="mt-1.5 block w-fit max-w-full overflow-hidden rounded-md border bg-background px-2 py-1 font-mono text-[11.5px]/[18px] whitespace-pre text-foreground select-all">
              {`sudo copper-cloud admin reset-admin-password \\\n  --email ${email.trim() || "you@example.com"}`}
            </code>
            {MOCK_MODE && (
              <p className="mt-3">Mock mode: any password works except “wrong”.</p>
            )}
          </div>
        </div>
      </main>
    </div>
  );
}
