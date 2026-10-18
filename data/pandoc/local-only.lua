-- Susen: used when pandoc's --sandbox is missing or broken. Drops images (and other
-- media) that would be read from outside the document's folder or fetched from the network,
-- so converting an untrusted document never embeds a local file or makes a request.
local function unsafe(src)
  return src:match('^%a[%w+.-]*:') ~= nil   -- any URL scheme: http:, file:, data: is harmless but rare
      or src:sub(1, 1) == '/' or src:sub(1, 1) == '\\'
      or src:sub(1, 1) == '~'
      or src:match('%.%.') ~= nil
end

function Image(el)
  if unsafe(el.src) then
    return el.caption
  end
end
