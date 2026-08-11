import { createFileRoute } from '@tanstack/react-router'
import {
  WakuDocsPage,
  docs,
  loadDocsPage,
} from '@/lib/docs'

export const Route = createFileRoute('/docs/$')({
  loader: async ({ params }) => {
    const data = await loadDocsPage({
      data: {
        locale: 'en',
        slugs: params._splat?.split('/').filter(Boolean) ?? [],
      },
    })
    await docs.getPage(data.path)?.preload()
    return data
  },
  head: ({ loaderData }) => ({
    meta: [
      { title: `${loaderData?.title ?? 'Docs'} — Waku` },
      ...(loaderData?.description
        ? [{ name: 'description', content: loaderData.description }]
        : []),
    ],
  }),
  component: EnglishDocsPage,
})

function EnglishDocsPage() {
  return <WakuDocsPage data={Route.useLoaderData()} locale="en" />
}
