#include "kiln_abi.h"

/* Both conversions are total over the doubles, so neither touches the error
 * slot: an error raised earlier must survive arithmetic that cannot fail. */

void units_c_to_f(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_double(ret, kn_arg_double(argv, 0) * 9.0 / 5.0 + 32.0);
}

void units_f_to_c(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_double(ret, (kn_arg_double(argv, 0) - 32.0) * 5.0 / 9.0);
}
