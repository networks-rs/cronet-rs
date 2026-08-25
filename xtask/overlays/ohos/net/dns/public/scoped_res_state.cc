// OHOS musl keeps __res_state for source compatibility but does not export
// res_ninit()/res_nclose(). Provide those two operations locally, then compile
// Chromium's unmodified ScopedResState implementation against them.

#include "net/dns/public/scoped_res_state.h"

#include <arpa/inet.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>

#include "base/compiler_specific.h"

namespace net {
namespace {

int CronetRsResNinit(struct __res_state* state) {
  UNSAFE_TODO(memset(state, 0, sizeof(*state)));
  state->retrans = RES_TIMEOUT;
  state->retry = RES_DFLRETRY;
  state->ndots = 1;
  state->options = RES_RECURSE | RES_DEFNAMES | RES_DNSRCH;

  FILE* file = fopen(_PATH_RESCONF, "r");
  if (!file) {
    return -1;
  }

  int search_count = 0;
  char* search_storage = state->defdname;
  size_t search_storage_left = sizeof(state->defdname);
  char line[512];
  while (fgets(line, sizeof(line), file)) {
    if (char* comment = strchr(line, '#')) {
      *comment = '\0';
    }
    char* save = nullptr;
    char* directive = strtok_r(line, " \t\r\n", &save);
    if (!directive) {
      continue;
    }

    if (strcmp(directive, "nameserver") == 0 && state->nscount < MAXNS) {
      char* address = strtok_r(nullptr, " \t\r\n", &save);
      if (!address) {
        continue;
      }
      const int index = state->nscount;
      auto& ipv4 = state->nsaddr_list[index];
      if (inet_pton(AF_INET, address, &ipv4.sin_addr) == 1) {
        ipv4.sin_family = AF_INET;
        ipv4.sin_port = htons(53);
        ++state->nscount;
        continue;
      }
      auto* ipv6 = new (std::nothrow) sockaddr_in6{};
      if (ipv6 && inet_pton(AF_INET6, address, &ipv6->sin6_addr) == 1) {
        ipv6->sin6_family = AF_INET6;
        ipv6->sin6_port = htons(53);
        state->_u._ext.nsaddrs[index] = ipv6;
        ++state->nscount;
      } else {
        delete ipv6;
      }
      continue;
    }

    if (strcmp(directive, "search") == 0 || strcmp(directive, "domain") == 0) {
      search_count = 0;
      search_storage = state->defdname;
      search_storage_left = sizeof(state->defdname);
      UNSAFE_TODO(memset(state->dnsrch, 0, sizeof(state->dnsrch)));
      while (search_count < MAXDNSRCH) {
        char* domain = strtok_r(nullptr, " \t\r\n", &save);
        if (!domain) {
          break;
        }
        const size_t length = strlen(domain) + 1;
        if (length > search_storage_left) {
          break;
        }
        UNSAFE_TODO(memcpy(search_storage, domain, length));
        state->dnsrch[search_count++] = search_storage;
        search_storage += length;
        search_storage_left -= length;
        if (strcmp(directive, "domain") == 0) {
          break;
        }
      }
      continue;
    }

    if (strcmp(directive, "options") == 0) {
      for (char* option = strtok_r(nullptr, " \t\r\n", &save); option;
           option = strtok_r(nullptr, " \t\r\n", &save)) {
        if (strncmp(option, "ndots:", 6) == 0) {
          state->ndots = std::min<unsigned long>(
              strtoul(option + 6, nullptr, 10), RES_MAXNDOTS);
        } else if (strncmp(option, "attempts:", 9) == 0) {
          state->retry = std::min<unsigned long>(
              strtoul(option + 9, nullptr, 10), RES_MAXRETRY);
        } else if (strncmp(option, "timeout:", 8) == 0) {
          state->retrans = std::min<unsigned long>(
              strtoul(option + 8, nullptr, 10), RES_MAXRETRANS);
        } else if (strcmp(option, "rotate") == 0) {
          state->options |= RES_ROTATE;
        }
      }
    }
  }
  fclose(file);

  if (state->nscount == 0) {
    return -1;
  }
  state->options |= RES_INIT;
  return 0;
}

void CronetRsResNclose(struct __res_state* state) {
  for (int i = 0; i < state->nscount; ++i) {
    if (!state->nsaddr_list[i].sin_family) {
      delete state->_u._ext.nsaddrs[i];
      state->_u._ext.nsaddrs[i] = nullptr;
    }
  }
}

}  // namespace
}  // namespace net

#define res_ninit CronetRsResNinit
#define res_nclose CronetRsResNclose
#include "net/dns/public/scoped_res_state_upstream.cc"
#undef res_nclose
#undef res_ninit
