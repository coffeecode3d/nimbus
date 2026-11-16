/*
 * Integration probe for app-based split tunneling.
 *
 * Sends three UDP datagrams to 1.0.0.1:53 four seconds after start, then idles.
 * The delay gives `nimbus-helper enforce` time to move this process into the
 * nimbus.apps cgroup, so the packets must be routed by table 100.
 *
 * Every send is checked: a silent failure (for example "no source address")
 * would otherwise look like "no traffic reached the tunnel".
 */
#include <arpa/inet.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    sleep(4);

    int s = socket(AF_INET, SOCK_DGRAM, 0);
    if (s < 0) {
        perror("probe: socket");
        return 1;
    }

    struct sockaddr_in to;
    memset(&to, 0, sizeof to);
    to.sin_family = AF_INET;
    to.sin_port = htons(53);
    inet_pton(AF_INET, "1.0.0.1", &to.sin_addr);

    int ok = 0;
    for (int i = 0; i < 3; i++) {
        ssize_t n = sendto(s, "nimbus-probe", 12, 0, (struct sockaddr *)&to, sizeof to);
        if (n < 0) {
            printf("probe: send %d failed: %s\n", i, strerror(errno));
        } else {
            ok++;
        }
        usleep(200000);
    }
    printf("probe: %d/3 datagrams sent\n", ok);
    fflush(stdout);

    sleep(30);
    return ok == 3 ? 0 : 1;
}
