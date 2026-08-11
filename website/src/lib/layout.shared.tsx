import { zhCN } from '@fumadocs/language/zh-cn'
import { uiTranslations } from 'fumadocs-ui/i18n'
import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared'
import { i18n, type DocsLocale } from '@/lib/i18n'

export const translations = i18n
  .translations()
  .extend(uiTranslations())
  .preset('zh', zhCN())
  .add({
    en: {
      displayName: 'English',
    },
    zh: {
      displayName: '简体中文',
    },
  })

export function baseOptions(locale: DocsLocale): BaseLayoutProps {
  const chinese = locale === 'zh'

  return {
    nav: {
      title: (
        <span className="flex items-center gap-2.5">
          <img
            src="/app-icon.png"
            alt=""
            className="size-7 rounded-[6px]"
          />
          <span>Waku</span>
        </span>
      ),
      url: chinese ? '/zh/docs' : '/docs',
    },
    links: [
      {
        text: chinese ? '首页' : 'Home',
        url: '/',
        active: 'none',
      },
    ],
    githubUrl: 'https://github.com/egoist/waku',
  }
}
