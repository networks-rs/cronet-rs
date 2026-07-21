#include <dlfcn.h>
#include "napi/native_api.h"

namespace {

using RunE2e = int (*)();

napi_value Run(napi_env env, napi_callback_info) {
  int result = 127;
  void* library = dlopen("libcronet_ohos_e2e_runner.so", RTLD_NOW | RTLD_LOCAL);
  if (library != nullptr) {
    auto run = reinterpret_cast<RunE2e>(
        dlsym(library, "cronet_rs_ohos_e2e_run"));
    if (run != nullptr) {
      result = run();
    }
    dlclose(library);
  }

  napi_value value = nullptr;
  napi_create_int32(env, result, &value);
  return value;
}

napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor properties[] = {
      {"run", nullptr, Run, nullptr, nullptr, nullptr, napi_default, nullptr},
  };
  napi_define_properties(env, exports, 1, properties);
  return exports;
}

napi_module module = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "entry",
    .nm_priv = nullptr,
    .reserved = {nullptr},
};

}  // namespace

extern "C" __attribute__((constructor)) void RegisterCronetE2eModule() {
  napi_module_register(&module);
}

