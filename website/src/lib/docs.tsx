import { Suspense, use, useCallback } from 'react'
import { notFound, useRouter, useRouterState } from '@tanstack/react-router'
import { createServerFn } from '@tanstack/react-start'
import { useFumadocsLoader } from 'fumadocs-core/source/client'
import { RootProvider } from 'fumadocs-ui/provider/tanstack'
import { i18nProvider } from 'fumadocs-ui/i18n'
import { DocsLayout } from 'fumadocs-ui/layouts/docs'
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
} from 'fumadocs-ui/layouts/docs/page'
import { getMDXComponents } from '@/components/mdx'
import type { DocsLocale } from '@/lib/i18n'
import { baseOptions, translations } from '@/lib/layout.shared'
import { docs, source } from '@/lib/source'

export { docs, source } from '@/lib/source'

type DocsPageRequest = {
  locale: DocsLocale
  slugs: string[]
}

export const loadDocsPage = createServerFn({ method: 'GET' })
  .validator((input: DocsPageRequest) => input)
  .handler(async ({ data: { locale, slugs } }) => {
    const page = source.getPage(slugs, locale)
    if (!page) throw notFound()

    return {
      path: page.path,
      title: page.data.title,
      description: page.data.description,
      pageTree: await source.serializePageTree(source.getPageTree(locale)),
    }
  })

export type DocsPageData = Awaited<ReturnType<typeof loadDocsPage>>

function Content({ path }: { path: string }) {
  const page = docs.getPage(path)
  if (!page) throw new Error(`Unknown documentation page: ${path}`)

  const { toc } = use(page.load())
  const MDX = page.body

  return (
    <DocsPage toc={toc}>
      <DocsTitle>{page.title}</DocsTitle>
      <DocsDescription>{page.description}</DocsDescription>
      <DocsBody>
        <MDX components={getMDXComponents()} />
      </DocsBody>
    </DocsPage>
  )
}

export function WakuDocsPage({
  data,
  locale,
}: {
  data: DocsPageData
  locale: DocsLocale
}) {
  const { path, pageTree } = useFumadocsLoader(data)
  const router = useRouter()
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  })
  const changeLocale = useCallback(
    (nextLocale: string) => {
      const englishPath = pathname.replace(/^\/zh(?=\/|$)/, '') || '/docs'
      const href = nextLocale === 'zh' ? `/zh${englishPath}` : englishPath
      void router.navigate({ href })
    },
    [pathname, router],
  )
  const provider = i18nProvider(translations, locale)

  return (
    <RootProvider
      i18n={{ ...provider, onLocaleChange: changeLocale }}
      theme={{ hotKey: false }}
    >
      <DocsLayout {...baseOptions(locale)} tree={pageTree}>
        <Suspense>
          <Content path={path} />
        </Suspense>
      </DocsLayout>
    </RootProvider>
  )
}
