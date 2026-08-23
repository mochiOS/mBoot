#!/usr/bin/env perl

use strict;
use warnings;
use Digest::SHA qw(sha256);
use FindBin;
use lib $FindBin::Bin;
use HvConfig qw(read_hv_config role_id);

my ($config_file, $output_file);
my %images;
while (@ARGV) {
    my $argument = shift @ARGV;
    if ($argument eq '--config') {
        $config_file = shift @ARGV;
    }
    elsif ($argument eq '--output') {
        $output_file = shift @ARGV;
    }
    elsif ($argument eq '--image') {
        my $mapping = shift @ARGV // '';
        $mapping =~ /^([a-z][a-z0-9_-]*)=(.+)$/
            or die "invalid --image mapping: $mapping\n";
        $images{$1} = $2;
    }
    else {
        die "unknown argument: $argument\n";
    }
}
defined $config_file && defined $output_file
    or die "usage: $0 --config FILE --image NAME=ELF --output FILE\n";

my $config = read_hv_config($config_file);
my $header_size = 32;
my $entry_size = 160;
my $domain_count = scalar @{$config->{domains}};
my $header = pack(
    'a8 v v v v V V Q<',
    "MBLHV1\0\0", $config->{version}, $header_size, $entry_size, $domain_count,
    $header_size + $entry_size * $domain_count, 0, 0,
);

my %image_bytes;
my $entries = '';
for my $domain (@{$config->{domains}}) {
    my $image_name = $domain->{image};
    exists $images{$image_name}
        or die "$config_file: no file was supplied for image '$image_name'\n";
    if (!exists $image_bytes{$image_name}) {
        open my $image_fh, '<:raw', $images{$image_name}
            or die "cannot read $images{$image_name}: $!\n";
        local $/;
        $image_bytes{$image_name} = <$image_fh>;
        close $image_fh or die "cannot close $images{$image_name}: $!\n";
    }
    my $flags = ($domain->{autostart} ? 1 : 0) | ($domain->{required} ? 2 : 0);
    my $path = $domain->{path};
    length($path) <= 80 or die "$config_file: Domain image path is too long\n";
    $entries .= pack(
        'V v v Q< v a14 Q< a32 v a6 a80',
        $domain->{id}, role_id($domain->{role}), $flags,
        $domain->{memory_mib} * 1024 * 1024, $domain->{vcpus}, '',
        $domain->{capabilities}, sha256($image_bytes{$image_name}),
        length($path), '', $path,
    );
}

open my $output_fh, '>:raw', $output_file or die "cannot write $output_file: $!\n";
print {$output_fh} $header, $entries or die "cannot write $output_file: $!\n";
close $output_fh or die "cannot close $output_file: $!\n";
