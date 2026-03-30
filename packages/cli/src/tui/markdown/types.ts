/** Theme configuration for markdown rendering */

// Smoo AI brand colors
const SMOO_GREEN = '#00a6a6';
const SMOO_ORANGE = '#f49f0a';

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
    heading1: { color: SMOO_ORANGE, bold: true },
    heading2: { color: SMOO_GREEN, bold: true },
    heading3: { color: 'white', bold: true },
    paragraph: {},
    bold: {},
    italic: {},
    code: { color: SMOO_GREEN },
    codeBlock: { color: 'gray', borderColor: SMOO_GREEN },
    listBullet: { color: SMOO_ORANGE },
    listNumber: { color: SMOO_ORANGE },
    link: { color: SMOO_GREEN },
    blockquote: { color: 'gray', borderColor: SMOO_GREEN },
    hr: { color: 'gray' },
};
