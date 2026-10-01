import { AppRail } from "@/components/app-rail";
import { MockBanner } from "@/components/mock-banner";
import { SessionProvider } from "@/components/session-provider";

export default function ConsoleLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <SessionProvider>
      <div className="flex h-dvh overflow-hidden">
        <AppRail />
        <main
          id="content"
          className="min-w-0 flex-1 scrollbar-gutter-stable overflow-y-auto"
        >
          <MockBanner />
          <div className="@container mx-auto w-full max-w-[1100px] px-6 pb-16 max-rail:px-5">
            {children}
          </div>
        </main>
      </div>
    </SessionProvider>
  );
}
