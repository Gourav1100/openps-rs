import java.lang.reflect.*;
import java.net.InetAddress;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Runs the ORIGINAL application's registration request builder (private method of com.grill.remoteplay.hb).
 * Reg <version> <PS4|PS5> <account id> <pin> [host=127.0.0.1]
 * Prints only the outcome (SUCCESS, or the HTTP status and the console's reason code): never keys or the account id.
 */
public class Reg {
    @SuppressWarnings({"unchecked", "rawtypes"})
    public static void main(String[] a) throws Exception {
        String version = a[0], device = a[1];
        long account = Long.parseUnsignedLong(a[2]);
        int pin = Integer.parseInt(a[3]);
        String host = a.length > 4 ? a[4] : "127.0.0.1";
        Class<?> hbc = Class.forName("com.grill.remoteplay.hb");
        Object h = hbc.getDeclaredConstructor().newInstance();
        Class<?> dev = Class.forName("com.grill.remoteplay.data.DeviceType");
        Object d = Enum.valueOf((Class) dev, device);
        CountDownLatch done = new CountDownLatch(1);
        Class<?> li = Class.forName("com.grill.remoteplay.registration.PSRegistrationListener");
        Object listener = Proxy.newProxyInstance(li.getClassLoader(), new Class<?>[]{li}, (p, m, args) -> {
            switch (m.getName()) {
                case "onPSRegistration" -> System.out.println("RESULT: SUCCESS (the console accepted the original application's request)");
                case "onPSRegistrationError" -> System.out.println("RESULT: HTTP " + args[0] + ", reason code '" + args[1] + "'");
                case "onPSNotAnswering" -> System.out.println("RESULT: console did not answer");
                default -> { }
            }
            done.countDown();
            return null;
        });
        hbc.getMethod("registerListener", li).invoke(h, listener);
        for (Method m : hbc.getDeclaredMethods()) {
            Class<?>[] p = m.getParameterTypes();
            if (p.length == 7 && p[0] == AtomicBoolean.class && p[2] == long.class) {
                m.setAccessible(true);
                m.invoke(h, new AtomicBoolean(false), "online", account, version, d, pin, InetAddress.getByName(host));
                done.await(15, TimeUnit.SECONDS);
                System.exit(0);
            }
        }
        throw new IllegalStateException("registration method not found");
    }
}
