// Minimal lexical highlighting for visible diff rows only: comments, strings,
// numbers and keywords. Per line, so a construct spanning lines is not tracked.
// Output is plain text segments (never HTML).
export type Segment = { text: string; cls: '' | 'k' | 's' | 'c' | 'n' };
type Family = { pattern: RegExp; keywords: Set<string> };

const MAX_CHARS = 400;
const STRINGS = String.raw`"(?:\\.|[^"\\])*"?|'(?:\\.|[^'\\])*'?`;
const NUMBER = String.raw`\b(?:0x[\da-fA-F_]+|\d[\d_]*(?:\.\d+)?)\b`;

const cLike: Family = {
  pattern: new RegExp(
    String.raw`(//.*|/\*.*?(?:\*/|$))|(${STRINGS}|` +
      '`(?:\\\\.|[^`\\\\])*`?' +
      String.raw`)|(${NUMBER})|([A-Za-z_$][\w$]*)`,
    'g',
  ),
  keywords: new Set(
    `as async await break case catch class const continue crate default do else enum export extends false fn for from func function go if impl import in interface let loop match mod mut new null package private protected pub public return self Self static struct super switch this throw true try type typeof undefined use var void where while yield`.split(
      ' ',
    ),
  ),
};
const hash: Family = {
  pattern: new RegExp(
    String.raw`(#.*)|(${STRINGS})|(${NUMBER})|([A-Za-z_][\w]*)`,
    'g',
  ),
  keywords: new Set(
    `and as assert async await break case class continue def del do done elif else esac except false False fi finally for from function if import in is lambda local None not or pass raise return then true True try until while with yield`.split(
      ' ',
    ),
  ),
};
const families: Record<string, Family> = {};
for (const ext of 'rs ts tsx js jsx mjs cjs svelte vue c h cc cpp hpp cxx java kt go cs swift scala dart php css scss less json jsonc'.split(
  ' ',
))
  families[ext] = cLike;
for (const ext of 'py sh bash zsh fish rb toml yaml yml conf cfg mk pl r'.split(
  ' ',
))
  families[ext] = hash;

export function familyFor(path: string): Family | null {
  const name = path.split('/').pop() ?? '';
  if (name === 'Makefile' || name === 'Dockerfile') return hash;
  const dot = name.lastIndexOf('.');
  return dot > 0 ? (families[name.slice(dot + 1).toLowerCase()] ?? null) : null;
}

export function highlight(text: string, family: Family | null): Segment[] {
  if (!family || text.length > MAX_CHARS) return [{ text, cls: '' }];
  const segments: Segment[] = [];
  let last = 0;
  family.pattern.lastIndex = 0;
  for (const match of text.matchAll(family.pattern)) {
    const [token, comment, string, number, word] = match;
    const cls = comment
      ? 'c'
      : string
        ? 's'
        : number
          ? 'n'
          : word && family.keywords.has(word)
            ? 'k'
            : '';
    if (!cls) continue;
    if (match.index > last)
      segments.push({ text: text.slice(last, match.index), cls: '' });
    segments.push({ text: token, cls });
    last = match.index + token.length;
  }
  if (last < text.length) segments.push({ text: text.slice(last), cls: '' });
  return segments;
}
