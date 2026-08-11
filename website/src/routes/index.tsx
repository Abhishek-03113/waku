import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { Menu } from '@base-ui/react/menu'
import {
  Command,
  Download,
  HardDrive,
  History,
  Layers,
  RefreshCw,
  Zap,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from '@/components/ui/accordion'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { FALLBACK_DOWNLOAD_URL, releaseQuery } from '@/lib/release'
import type { ReactNode } from 'react'

export const Route = createFileRoute('/')({
  loader: ({ context }) => {
    // Fire-and-forget: the version chip streams in when the appcast answers.
    void context.queryClient.prefetchQuery(releaseQuery)
  },
  component: Home,
})

const PROVIDERS = [
  { slug: 'amp', label: 'Amp' },
  { slug: 'claude', label: 'Claude Code' },
  { slug: 'openai', label: 'Codex' },
  { slug: 'cursor', label: 'Cursor' },
  { slug: 'opencode', label: 'OpenCode' },
  { slug: 'grok', label: 'Grok' },
  { slug: 'pi', label: 'Pi' },
]

const FEATURES = [
  {
    icon: Zap,
    title: 'Native down to the frame',
    body: 'Rust and GPUI — the GPU-accelerated framework behind Zed. Instant launch, smooth scrolling through years of transcript, no Electron.',
  },
  {
    icon: Layers,
    title: 'Every agent, one timeline',
    body: 'Each agent is connected over its strongest native interface — stream-json, JSON-RPC, live events — and normalized into one provider-neutral model.',
  },
  {
    icon: History,
    title: 'Rewind that means it',
    body: 'Every prompt checkpoints your working tree under a hidden git ref. Roll back the code and the provider conversation together, not just the chat log.',
  },
  {
    icon: Command,
    title: 'Keyboard first',
    body: '⌘N starts a session, ⏎ queues a follow-up while the agent works, ⌘⏎ steers it mid-turn, Escape stops. Every control works without a mouse.',
  },
  {
    icon: HardDrive,
    title: 'Local by architecture',
    body: 'Projects, sessions, transcripts, and provider IDs live on your disk. No account, no telemetry, no Waku cloud between you and your agents.',
  },
  {
    icon: RefreshCw,
    title: 'Quietly current',
    body: 'Signed, notarized, and auto-updated with binary deltas via Sparkle. The app stays fresh without asking for your attention.',
  },
]

const FAQ = [
  {
    q: 'Is this another Electron app?',
    a: 'No. Waku is a single Rust binary rendered by GPUI, the UI framework Zed is built on. The window you see is drawn by the GPU, not by a browser engine.',
  },
  {
    q: 'Do I need new API keys?',
    a: 'No. Waku detects amp, claude, codex, cursor-agent, opencode, grok, and pi on your machine and drives them directly — your existing logins, plans, and rate limits apply unchanged.',
  },
  {
    q: 'Where does my data live?',
    a: 'On your Mac. Projects, sessions, transcripts, and provider session IDs are stored locally. There is no Waku account and no telemetry.',
  },
  {
    q: 'What about Windows and Linux?',
    a: "Waku's core is cross-platform Rust. macOS ships first; Windows and Linux builds are planned.",
  },
  {
    q: 'What is the future plan?',
    a: 'A mobile app for remote control, and cloud agents are planned',
  },
]

function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <div className="font-mono text-[11px] tracking-[0.14em] text-muted-foreground/80 uppercase">
      {children}
    </div>
  )
}

function DownloadMenu({
  downloadUrl,
  size,
  align,
  className,
  showIcon = false,
}: {
  downloadUrl: string
  size: 'sm' | 'lg'
  align: 'start' | 'end'
  className?: string
  showIcon?: boolean
}) {
  const itemClassName =
    'flex h-8 cursor-default items-center rounded-md px-2.5 text-sm outline-none data-highlighted:bg-accent data-highlighted:text-accent-foreground data-disabled:pointer-events-none data-disabled:opacity-45'

  return (
    <Menu.Root>
      <Menu.Trigger render={<Button size={size} className={className} />}>
        {showIcon && <Download data-icon="inline-start" />}
        Download
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner
          side="bottom"
          sideOffset={6}
          align={align}
          className="isolate z-50"
        >
          <Menu.Popup className="min-w-52 origin-(--transform-origin) rounded-lg border bg-popover p-1 text-popover-foreground shadow-md outline-none data-[side=bottom]:slide-in-from-top-1 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95">
            <Menu.LinkItem
              href={downloadUrl}
              closeOnClick
              className={itemClassName}
            >
              macOS (Apple Silicon)
            </Menu.LinkItem>
            <Menu.Item disabled className={itemClassName}>
              Windows (soon)
            </Menu.Item>
            <Menu.Item disabled className={itemClassName}>
              Linux (soon)
            </Menu.Item>
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  )
}

function Home() {
  const { data: release } = useQuery(releaseQuery)
  const downloadUrl = release?.url ?? FALLBACK_DOWNLOAD_URL

  return (
    <TooltipProvider>
      <div className="min-h-dvh antialiased">
        <div className="mx-auto w-full max-w-[1100px] border-border/70 md:border-x">
          {/* Header */}
          <header className="flex h-16 items-center justify-between px-5 md:px-10">
            <a href="/" className="flex items-center gap-2.5">
              <img
                src="/app-icon.png"
                alt=""
                className="size-8 rounded-[6px]"
              />
              <span className="text-[15px] font-semibold tracking-tight">
                Waku
              </span>
            </a>
            <DownloadMenu
              downloadUrl={downloadUrl}
              size="sm"
              align="end"
            />
          </header>

          <main>
            {/* Hero */}
            <section className="px-5 pt-14 pb-14 md:px-10 md:pt-24">
              <div className="mb-7 inline-flex items-center gap-2 rounded-full border px-3 py-1 text-xs text-muted-foreground">
                <span className="flex size-3.5 items-center justify-center rounded-[3px] bg-[#f26522] text-[10px] font-bold text-white">
                  Y
                </span>
                Not backed by Y Combinator
              </div>
              <h1 className="max-w-4xl text-4xl font-semibold tracking-[-0.03em] text-balance md:text-[3.4rem] md:leading-[1.04]">
                One native app for all your coding agents.
              </h1>
              <p className="mt-5 max-w-[36rem] text-[17px] leading-relaxed text-pretty text-muted-foreground">
                Waku drives the agent CLIs you already have — sessions,
                transcripts, tool activity, and checkpoints in one fast
                graphite window, entirely on your machine.
              </p>
              <div className="mt-8 flex flex-wrap items-center gap-x-5 gap-y-3">
                <DownloadMenu
                  downloadUrl={downloadUrl}
                  size="lg"
                  className="h-10 px-4"
                  align="start"
                  showIcon
                />
                {release && (
                  <span className="font-mono text-xs text-muted-foreground">
                    v{release.version}
                  </span>
                )}
              </div>

              {/* Providers */}
              <div className="mt-16">
                <SectionLabel>Drives the agents you already use</SectionLabel>
                <div className="mt-4 flex flex-wrap items-center gap-x-7 gap-y-4">
                  {PROVIDERS.map((p) => (
                    <Tooltip key={p.slug}>
                      <TooltipTrigger
                        render={
                          <button
                            type="button"
                            aria-label={p.label}
                            className="cursor-default rounded-sm text-muted-foreground/70 transition-colors outline-none hover:text-foreground focus-visible:text-foreground focus-visible:ring-2 focus-visible:ring-ring/60"
                          />
                        }
                      >
                        <span
                          className="provider-mark size-[22px]"
                          style={{
                            maskImage: `url(/providers/${p.slug}.svg)`,
                            WebkitMaskImage: `url(/providers/${p.slug}.svg)`,
                          }}
                        />
                      </TooltipTrigger>
                      <TooltipContent>{p.label}</TooltipContent>
                    </Tooltip>
                  ))}
                </div>
              </div>
            </section>

            {/* Product */}
            <section>
              <picture>
                <source
                  media="(prefers-color-scheme: dark)"
                  srcSet="/app-screenshot-dark.png"
                />
                <img
                  src="/app-screenshot-light.png"
                  alt="Waku showing a coding-agent session"
                  width={2266}
                  height={1752}
                  className="block h-auto w-full"
                />
              </picture>
            </section>

            {/* Features */}
            <section className="border-t">
              <div className="px-5 pt-14 md:px-10">
                <SectionLabel>Why native</SectionLabel>
              </div>
              <div className="mt-8 grid grid-cols-1 gap-px border-t bg-border/70 sm:grid-cols-2 lg:grid-cols-3">
                {FEATURES.map((f) => (
                  <div key={f.title} className="bg-background p-6 md:p-8">
                    <div className="flex items-center gap-2.5">
                      <f.icon className="size-4 text-muted-foreground" />
                      <h3 className="text-sm font-medium">{f.title}</h3>
                    </div>
                    <p className="mt-2.5 text-sm leading-relaxed text-muted-foreground">
                      {f.body}
                    </p>
                  </div>
                ))}
              </div>
            </section>

            {/* Download */}
            <section id="download" className="border-t px-5 py-16 md:px-10 md:py-20">
              <SectionLabel>Download</SectionLabel>
              <h2 className="mt-3 text-2xl font-semibold tracking-tight">
                Get Waku
              </h2>
              <div className="mt-6 flex flex-wrap items-center gap-x-5 gap-y-3">
                <DownloadMenu
                  downloadUrl={downloadUrl}
                  size="lg"
                  className="h-10 px-4"
                  align="start"
                  showIcon
                />
                {release && (
                  <span className="font-mono text-xs text-muted-foreground">
                    v{release.version}
                  </span>
                )}
              </div>
            </section>

            {/* FAQ */}
            <section className="border-t px-5 py-16 md:px-10">
              <SectionLabel>Questions</SectionLabel>
              <Accordion className="mt-6 max-w-2xl">
                {FAQ.map((item) => (
                  <AccordionItem key={item.q} value={item.q}>
                    <AccordionTrigger className="text-[15px]">
                      {item.q}
                    </AccordionTrigger>
                    <AccordionContent className="max-w-[38rem] text-muted-foreground">
                      {item.a}
                    </AccordionContent>
                  </AccordionItem>
                ))}
              </Accordion>
            </section>
          </main>

          {/* Footer */}
          <footer className="flex items-center gap-2 border-t px-5 py-10 text-xs text-muted-foreground md:px-10">
            <img
              src="/app-icon.png"
              alt=""
              className="size-4 rounded-[4px] opacity-80 grayscale"
            />
            <span>© 2026 Waku</span>
          </footer>
        </div>
      </div>
    </TooltipProvider>
  )
}
