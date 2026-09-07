// A C++ host for the library. Build the archive first — the header lands
// beside it — then compile this against that header and link the archive:
//
//   kiln build main.kiln -o lib__MODULE__.a
//   clang++ consumer.cpp -I. lib__MODULE__.a -lm -o consumer && ./consumer
//
// For Windows, cross-built from Linux (mingw keeps the Unix archive name):
//
//   kiln build main.kiln --os windows -o lib__MODULE__.a
//   x86_64-w64-mingw32-g++ consumer.cpp lib__MODULE__.a -lws2_32 -o consumer.exe
//
// The archive was built by mingw, so it links with mingw; MSVC's link wants a
// library its own toolchain built. The same file compiles as C
// (`clang -x c consumer.cpp ...`): the header wraps its prototypes in
// `extern "C"` only when a C++ compiler reads it.
#include <stdio.h>
#include "__MODULE__.h"

int main(void) {
    __MODULE___init();                     /* module variables, once, first */
    greet();
    printf("%d\n", (int)add(2, 3));
    printf("%s\n", greeting("world"));      /* the text belongs to the library */
    return 0;
}
