package hither.android;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Locale;
import java.util.Map;

final class Http {
    final String method;
    final String path;
    final Map<String, String> query;
    final Map<String, String> headers;
    final InputStream body;

    private Http(String method, String path, Map<String, String> query, Map<String, String> headers, InputStream body) {
        this.method = method;
        this.path = path;
        this.query = query;
        this.headers = headers;
        this.body = body;
    }

    static Http read(InputStream in) throws IOException {
        String first = line(in);
        if (first == null || first.isEmpty()) return null;
        String[] parts = first.split(" ");
        if (parts.length < 2) throw new IOException("bad request line");
        Map<String, String> headers = new HashMap<>();
        int total = 0;
        String l;
        while ((l = line(in)) != null && !l.isEmpty()) {
            total += l.length();
            if (total > 16384) throw new IOException("headers too large");
            int i = l.indexOf(':');
            if (i > 0) headers.put(l.substring(0, i).trim().toLowerCase(Locale.ROOT), l.substring(i + 1).trim());
        }
        String target = parts[1];
        int q = target.indexOf('?');
        String path = q < 0 ? target : target.substring(0, q);
        Map<String, String> query = new HashMap<>();
        if (q >= 0) {
            for (String kv : target.substring(q + 1).split("&")) {
                int e = kv.indexOf('=');
                if (e > 0) query.put(decode(kv.substring(0, e)), decode(kv.substring(e + 1)));
            }
        }
        InputStream body;
        if ("chunked".equalsIgnoreCase(headers.get("transfer-encoding"))) {
            body = new Chunked(in);
        } else {
            String cl = headers.get("content-length");
            long len;
            try {
                len = cl == null ? 0 : Long.parseLong(cl);
            } catch (NumberFormatException e) {
                throw new IOException("bad content length");
            }
            body = new Limited(in, len);
        }
        return new Http(parts[0], path, query, headers, body);
    }

    byte[] bytes(int max) throws IOException {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        byte[] buf = new byte[8192];
        int n;
        while ((n = body.read(buf)) > 0) {
            if (b.size() + n > max) throw new IOException("request too large");
            b.write(buf, 0, n);
        }
        return b.toByteArray();
    }

    static void reply(OutputStream out, int code, String json) throws IOException {
        byte[] b = json == null ? new byte[0] : json.getBytes(StandardCharsets.UTF_8);
        String head = "HTTP/1.1 " + code + " " + reason(code) + "\r\nContent-Type: application/json\r\nContent-Length: " + b.length
                + "\r\nConnection: close\r\n\r\n";
        out.write(head.getBytes(StandardCharsets.ISO_8859_1));
        out.write(b);
        out.flush();
    }

    private static String reason(int code) {
        switch (code) {
            case 200: return "OK";
            case 204: return "No Content";
            case 400: return "Bad Request";
            case 403: return "Forbidden";
            case 404: return "Not Found";
            case 409: return "Conflict";
            case 413: return "Payload Too Large";
            default: return "Error";
        }
    }

    private static String decode(String s) {
        try {
            return URLDecoder.decode(s, "UTF-8");
        } catch (IOException | IllegalArgumentException e) {
            return s;
        }
    }

    private static String line(InputStream in) throws IOException {
        StringBuilder sb = new StringBuilder();
        int c;
        while ((c = in.read()) != -1) {
            if (c == '\n') return sb.toString();
            if (c != '\r') sb.append((char) c);
            if (sb.length() > 8192) throw new IOException("line too long");
        }
        return sb.length() == 0 ? null : sb.toString();
    }

    private static final class Limited extends InputStream {
        private final InputStream in;
        private long left;

        Limited(InputStream in, long len) {
            this.in = in;
            left = len;
        }

        @Override
        public int read() throws IOException {
            byte[] one = new byte[1];
            return read(one, 0, 1) <= 0 ? -1 : one[0] & 0xff;
        }

        @Override
        public int read(byte[] b, int off, int len) throws IOException {
            if (left <= 0) return -1;
            int n = in.read(b, off, (int) Math.min(len, left));
            if (n > 0) left -= n;
            return n;
        }
    }

    private static final class Chunked extends InputStream {
        private final InputStream in;
        private long left;
        private boolean done;

        Chunked(InputStream in) {
            this.in = in;
        }

        @Override
        public int read() throws IOException {
            byte[] one = new byte[1];
            return read(one, 0, 1) <= 0 ? -1 : one[0] & 0xff;
        }

        @Override
        public int read(byte[] b, int off, int len) throws IOException {
            if (done) return -1;
            if (left == 0) {
                String size = line(in);
                if (size == null) throw new IOException("chunk cut short");
                int semi = size.indexOf(';');
                try {
                    left = Long.parseLong((semi < 0 ? size : size.substring(0, semi)).trim(), 16);
                } catch (NumberFormatException e) {
                    throw new IOException("bad chunk size");
                }
                if (left == 0) {
                    String t;
                    while ((t = line(in)) != null && !t.isEmpty()) {
                    }
                    done = true;
                    return -1;
                }
            }
            int n = in.read(b, off, (int) Math.min(len, left));
            if (n < 0) throw new IOException("chunk cut short");
            left -= n;
            if (left == 0) line(in);
            return n;
        }
    }
}
