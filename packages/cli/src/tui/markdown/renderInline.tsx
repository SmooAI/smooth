import type { Token, Tokens } from 'marked';

/** Render inline markdown tokens into Ink Text components */

import { Text } from 'ink';
import React from 'react';

import type { MarkdownTheme } from './types.js';

export function renderInlineTokens(tokens: Token[], theme: MarkdownTheme, key = 0): React.ReactNode[] {
    return tokens.map((token, i) => renderInlineToken(token, theme, key * 1000 + i));
}

function renderInlineToken(token: Token, theme: MarkdownTheme, key: number): React.ReactNode {
    switch (token.type) {
        case 'text':
            return <Text key={key}>{(token as Tokens.Text).text}</Text>;

        case 'strong':
            return (
                <Text key={key} bold color={theme.bold.color}>
                    {renderInlineTokens((token as Tokens.Strong).tokens, theme, key)}
                </Text>
            );

        case 'em':
            return (
                <Text key={key} italic color={theme.italic.color}>
                    {renderInlineTokens((token as Tokens.Em).tokens, theme, key)}
                </Text>
            );

        case 'codespan':
            return (
                <Text key={key} color={theme.code.color}>
                    {(token as Tokens.Codespan).text}
                </Text>
            );

        case 'link': {
            const link = token as Tokens.Link;
            return (
                <Text key={key} color={theme.link.color} underline>
                    {link.text}
                    <Text dimColor> ({link.href})</Text>
                </Text>
            );
        }

        case 'br':
            return <Text key={key}>{'\n'}</Text>;

        case 'del':
            return (
                <Text key={key} strikethrough>
                    {renderInlineTokens((token as Tokens.Del).tokens, theme, key)}
                </Text>
            );

        case 'escape':
            return <Text key={key}>{(token as Tokens.Escape).text}</Text>;

        default:
            if ('text' in token) {
                return <Text key={key}>{(token as { text: string }).text}</Text>;
            }
            return null;
    }
}
