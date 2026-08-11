import { createFileRoute } from '@tanstack/react-router'
import {
  WakuDocsPage,
  docs,
  loadDocsPage,
} from '@/lib/docs'

export const Route = createFileRoute('/zh/docs/$')({
  loader: async ({ params }) => {
    const data = await loadDocsPage({
      data: {
        locale: 'zh',
        slugs: params._splat?.split('/').filter(Boolean) ?? [],
      },
    })
    await docs.getPage(data.path)?.preload()
    return data
  },
  head: ({ loaderData }) => ({
    meta: [
      { title: `${loaderData?.title ?? '文档'} — Waku` },
      ...(loaderData?.description
        ? [{ name: 'description', content: loaderData.description }]
        : []),
    ],
  }),
  component: ChineseDocsPage,
})

function ChineseDocsPage() {
  return <WakuDocsPage data={Route.useLoaderData()} locale="zh" />
}
