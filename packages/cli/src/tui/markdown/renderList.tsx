import type { Tokens } from 'marked';

/** Render list tokens into Ink components */

import { Box, Text } from 'ink';
import React from 'react';

import type { MarkdownTheme } from './types.js';

import { renderInlineTokens } from './renderInline.js';
import { renderToken } from './renderMarkdown.js';

export function renderList(token: Tokens.List, theme: MarkdownTheme, key: number): React.ReactNode {
    return (
        <Box key={key} flexDirection="column" marginBottom={1}>
            {token.items.map((item, i) => renderListItem(item, theme, i, token.ordered, token.start))}
        </Box>
    );
}

function renderListItem(item: Tokens.ListItem, theme: MarkdownTheme, index: number, ordered: boolean, start: number | ''): React.ReactNode {
    const bullet = ordered ? <Text color={theme.listNumber.color}>{`${(start || 1) + index}. `}</Text> : <Text color={theme.listBullet.color}>{'  ● '}</Text>;

    // List items can contain nested blocks (paragraphs, sublists)
    const content = item.tokens.map((t, i) => {
        if (t.type === 'text' && 'tokens' in t && (t as Tokens.Text).tokens) {
            return <Text key={i}>{renderInlineTokens((t as Tokens.Text).tokens!, theme, i)}</Text>;
        }
        if (t.type === 'list') {
            return (
                <Box key={i} marginLeft={2}>
                    {renderList(t as Tokens.List, theme, i)}
                </Box>
            );
        }
        return renderToken(t, theme, i);
    });

    return (
        <Box key={index}>
            {bullet}
            <Box flexDirection="column" flexShrink={1}>
                {content}
            </Box>
        </Box>
    );
}
