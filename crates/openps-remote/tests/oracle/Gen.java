import java.lang.reflect.*;
import java.net.InetAddress;
import java.nio.file.*;
import java.util.*;

/**
 * Runs the ORIGINAL application's own classes (com.grill.remoteplay.{gb,gc,fg,ff,gz}) on seeded inputs and records exact outputs.
 *   Gen <seed> <count> <out-public.jsonl> <out-exact.jsonl>
 * public file: inputs and outputs WITHOUT the cipher nonce (safe to commit: the nonce would reveal key-table bytes through the patch).
 * exact file : the same plus the nonce, for byte-exact comparison on a machine that has the original app (git-ignored).
 */
public class Gen {
    static String hex(byte[] b) { StringBuilder s = new StringBuilder(); for (byte x : b) s.append(String.format("%02x", x)); return s.toString(); }
    static Object call(Class<?> c, String name, Class<?>[] types, Object... args) throws Exception {
        Method m = c.getDeclaredMethod(name, types); m.setAccessible(true); return m.invoke(null, args);
    }
    static byte[] rnd(Random r, int n) { byte[] b = new byte[n]; r.nextBytes(b); return b; }

    public static void main(String[] a) throws Exception {
        long seed = Long.parseLong(a[0]); int count = Integer.parseInt(a[1]);
        Random r = new Random(seed);
        String P = "com.grill.remoteplay.";
        Class<?> gb = Class.forName(P + "gb"), gc = Class.forName(P + "gc"), fg = Class.forName(P + "fg"), ff = Class.forName(P + "ff"), gz = Class.forName(P + "gz"), fj = Class.forName(P + "fj");
        StringBuilder pub = new StringBuilder(), exact = new StringBuilder();
        String alpha = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
        for (int n = 0; n < count; n++) {
            // 1. customData1 -> l  (gb.a(i, j, ps5).a(raw, 0))
            byte[] i = new byte[16], j = new byte[16];
            for (int k = 0; k < 16; k++) { i[k] = (byte) alpha.charAt(r.nextInt(32)); j[k] = (byte) alpha.charAt(r.nextInt(32)); }
            byte[] raw = rnd(r, 16);
            Object cc = call(gb, "a", new Class<?>[]{byte[].class, byte[].class, boolean.class}, i, j, true);
            byte[] l = (byte[]) gc.getMethod("a", byte[].class, int.class).invoke(cc, raw, 0);
            pub.append("{\"kind\":\"custom\",\"i\":\"" + hex(i) + "\",\"j\":\"" + hex(j) + "\",\"raw\":\"" + hex(raw) + "\",\"l\":\"" + hex(l) + "\"}\n");

            // 2. register request (ff over fg over gb.a(l, idx))
            byte[] body = rnd(r, 480); int idx = body[397] & 0x1F;
            long account = r.nextLong() >>> 1; if (account == 0) account = 1;
            InetAddress ip = InetAddress.getByAddress(new byte[]{(byte) 192, (byte) 168, (byte) r.nextInt(256), (byte) (1 + r.nextInt(254))});
            Object reg = call(gb, "a", new Class<?>[]{byte[].class, int.class}, l, idx);
            Constructor<?> fgc = fg.getDeclaredConstructors()[0]; fgc.setAccessible(true);
            Object fgo = fgc.newInstance(reg, body, true, account, ip);
            Constructor<?> ffc = ff.getDeclaredConstructors()[0]; ffc.setAccessible(true);
            Object ffo = ffc.newInstance(fgo);
            byte[] req = (byte[]) fj.getMethod("a").invoke(ffo);
            Field nf = gc.getField("a"); byte[] nonce = (byte[]) nf.get(reg);
            String common = "\"l\":\"" + hex(l) + "\",\"body\":\"" + hex(body) + "\",\"account\":\"" + Long.toUnsignedString(account) + "\",\"ip\":\"" + ip.getHostAddress() + "\",\"request\":\"" + hex(req) + "\"";
            pub.append("{\"kind\":\"register\"," + common + "}\n");
            exact.append("{\"kind\":\"register\"," + common + ",\"nonce\":\"" + hex(nonce) + "\"}\n");

            // 3. RUDP datagram builders (gz)
            int seq = 511 + r.nextInt(20000), tag = 32768 + r.nextInt(1 << 30), n3 = r.nextInt(60000);
            byte[] cookie = rnd(r, 8 + r.nextInt(24));
            Object proxy = Proxy.newProxyInstance(fj.getClassLoader(), new Class<?>[]{fj}, (p, m, args) -> req);
            byte[] init = (byte[]) call(gz, "a", new Class<?>[]{int.class, int.class}, seq, tag);
            byte[] ck = (byte[]) call(gz, "a", new Class<?>[]{int.class, int.class, byte[].class}, seq, tag, cookie);
            byte[] rq = (byte[]) call(gz, "a", new Class<?>[]{int.class, int.class, fj}, seq, n3, proxy);
            byte[] fin = (byte[]) call(gz, "b", new Class<?>[]{int.class, int.class}, seq, n3);
            pub.append("{\"kind\":\"rudp\",\"seq\":" + seq + ",\"tag\":" + tag + ",\"n3\":" + n3 + ",\"cookie\":\"" + hex(cookie) + "\",\"request\":\"" + hex(req) + "\",\"init\":\"" + hex(init) + "\",\"cookie_dg\":\"" + hex(ck) + "\",\"request_dg\":\"" + hex(rq) + "\",\"final\":\"" + hex(fin) + "\"}\n");
        }
        Files.write(Paths.get(a[2]), pub.toString().getBytes("UTF-8"));
        Files.write(Paths.get(a[3]), exact.toString().getBytes("UTF-8"));
        System.out.println("wrote " + count + " cases");
        System.exit(0);
    }
}
