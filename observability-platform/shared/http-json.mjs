export function createHttpError(statusCode, status, message) {
  const error = new Error(message);
  error.statusCode = statusCode;
  error.googleStatus = status;
  return error;
}

export async function readLimitedJsonBody(req, maxBodyBytes) {
  const chunks = [];
  let totalBytes = 0;

  for await (const chunk of req) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    totalBytes += buffer.length;
    if (totalBytes > maxBodyBytes) {
      throw createHttpError(413, "INVALID_ARGUMENT", `Request body too large (max ${maxBodyBytes} bytes)`);
    }
    chunks.push(buffer);
  }

  if (chunks.length === 0) {
    return {};
  }

  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {
    return {};
  }

  return JSON.parse(raw);
}
