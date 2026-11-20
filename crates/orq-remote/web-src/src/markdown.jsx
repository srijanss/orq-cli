// A small markdown renderer. The text is turned into React elements, never into HTML, so nothing in it
// is ever interpreted as markup: anything it does not understand shows as plain text.
const HEADING = /^(#{1,4})\s+(.*)$/;
const LIST_ITEM = /^(\s*)([-*]|\d+\.)\s+(.*)$/;

// The first inline mark in a line: `code`, **bold**, *italic* or [label](address).
const INLINE = /`([^`]+)`|\*\*(.+?)\*\*|\*([^*\s](?:[^*]*[^*\s])?)\*|\[([^\][]+)\]\(([^)\s]+)\)/;
// The only addresses a link may have.
const WEB_ADDRESS = /^https?:\/\//i;

// The elements of one line of text. Text between marks stays text.
function inline(text) {
  const parts = [];
  let rest = text;
  for (let key = 0; rest !== ""; key += 1) {
    const mark = INLINE.exec(rest);
    if (mark === null) {
      parts.push(rest);
      break;
    }
    if (mark.index > 0) parts.push(rest.slice(0, mark.index));
    if (mark[1] !== undefined) parts.push(<code key={key}>{mark[1]}</code>);
    else if (mark[2] !== undefined) parts.push(<strong key={key}>{inline(mark[2])}</strong>);
    else if (mark[3] !== undefined) parts.push(<em key={key}>{inline(mark[3])}</em>);
    // A link to anything but http or https stays as written.
    else if (!WEB_ADDRESS.test(mark[5])) parts.push(mark[0]);
    else parts.push(<a key={key} href={mark[5]} target="_blank" rel="noopener noreferrer">{mark[4]}</a>);
    rest = rest.slice(mark.index + mark[0].length);
  }
  return parts;
}

// A list from its item lines, nested by indent: [{ordered, items: [{text, children: [list]}]}].
function nest(items) {
  const root = { ordered: items[0].ordered, items: [] };
  const stack = [{ indent: items[0].indent, list: root }];
  for (const item of items) {
    while (stack.length > 1 && item.indent < stack[stack.length - 1].indent) stack.pop();
    let top = stack[stack.length - 1];
    // A nested item of another kind than its list's starts a sibling list of its own kind.
    if (stack.length > 1 && item.indent === top.indent && item.ordered !== top.list.ordered) {
      stack.pop();
      top = stack[stack.length - 1];
      const inner = { ordered: item.ordered, items: [] };
      top.list.items[top.list.items.length - 1].children.push(inner);
      top = { indent: item.indent, list: inner };
      stack.push(top);
    } else if (item.indent > top.indent) {
      const inner = { ordered: item.ordered, items: [] };
      const parent = top.list.items[top.list.items.length - 1];
      parent.children.push(inner);
      top = { indent: item.indent, list: inner };
      stack.push(top);
    }
    top.list.items.push({ text: item.text, children: [] });
  }
  return root;
}

// The blocks of `text`: headings, lists and paragraphs, split at blank lines.
function blocks(text) {
  const found = [];
  let paragraph = [];
  let list = [];
  const flush = () => {
    if (paragraph.length > 0) found.push({ kind: "p", text: paragraph.join(" ") });
    if (list.length > 0) found.push({ kind: "list", list: nest(list) });
    paragraph = [];
    list = [];
  };
  // The lines of the fenced block being read, or null outside one.
  let fence = null;
  for (const line of text.split("\n")) {
    const heading = HEADING.exec(line);
    const item = LIST_ITEM.exec(line);
    if (fence !== null) {
      if (line.trim().startsWith("```")) {
        found.push({ kind: "code", text: fence.join("\n") });
        fence = null;
      } else {
        fence.push(line);
      }
    } else if (line.trim().startsWith("```")) {
      flush();
      fence = [];
    } else if (heading !== null) {
      flush();
      found.push({ kind: "h", level: heading[1].length, text: heading[2] });
    } else if (item !== null) {
      const ordered = /^\d/.test(item[2]);
      // A different kind of list at the top level starts a new list.
      if (list.length > 0 && list[0].ordered !== ordered && item[1].length <= list[0].indent) flush();
      if (paragraph.length > 0) flush();
      list.push({ indent: item[1].length, ordered, text: item[3] });
    } else if (line.trim() === "") {
      if (paragraph.length > 0) flush();
    } else {
      if (list.length > 0) flush();
      paragraph.push(line.trim());
    }
  }
  // A fence that is never closed runs to the end of the text.
  if (fence !== null) found.push({ kind: "code", text: fence.join("\n") });
  flush();
  return found;
}

function List({ list }) {
  const Tag = list.ordered ? "ol" : "ul";
  return (
    <Tag>
      {list.items.map((item, index) => (
        <li key={index}>
          {inline(item.text)}
          {item.children.map((inner, at) => (
            <List key={at} list={inner} />
          ))}
        </li>
      ))}
    </Tag>
  );
}

export function Markdown({ text }) {
  return (
    <div className="markdown">
      {blocks(text).map((block, index) => {
        if (block.kind === "h") {
          // Headings show at the page's h3 and h4 sizes, never larger than its h1.
          const Tag = block.level <= 2 ? "h3" : "h4";
          return <Tag key={index}>{inline(block.text)}</Tag>;
        }
        if (block.kind === "list") return <List key={index} list={block.list} />;
        if (block.kind === "code") return <pre key={index}><code>{block.text}</code></pre>;
        return <p key={index}>{inline(block.text)}</p>;
      })}
    </div>
  );
}
