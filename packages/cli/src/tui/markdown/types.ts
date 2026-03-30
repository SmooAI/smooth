/** Theme configuration for markdown rendering */

export interface MarkdownTheme {
    heading1: { color: string; bold: boolean };
    heading2: { color: string; bold: boolean };
    heading3: { color: string; bold: boolean };
    paragraph: { color?: string };
    bold: { color?: string };
    italic: { color?: string };
    code: { color: string };
    codeBlock: { color: string; borderColor: string };
    listBullet: { color: string };
    listNumber: { color: string };
    link: { color: string };
    blockquote: { color: string; borderColor: string };
    hr: { color: string };
}

export const DEFAULT_THEME: MarkdownTheme = {
    heading1: { color: 'cyan', bold: true },
    heading2: { color: 'blue', bold: true },
    heading3: { color: 'white', bold: true },
    paragraph: {},
    bold: {},
    italic: {},
    code: { color: 'cyan' },
    codeBlock: { color: 'gray', borderColor: 'gray' },
    listBullet: { color: 'cyan' },
    listNumber: { color: 'cyan' },
    link: { color: 'blue' },
    blockquote: { color: 'gray', borderColor: 'gray' },
    hr: { color: 'gray' },
};
